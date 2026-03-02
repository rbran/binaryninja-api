use std::ffi::OsStr;
use std::path::Path;
use std::{collections::HashMap, path::PathBuf};

use clang::{Clang, EntityKind, Index};
use serde::Deserialize;
use syn::ExprCall;
use syn::visit::Visit;

fn main() {
    let functions = get_function_coreapi();

    let c = count_c(&functions);
    let python = count_python(&functions);
    let rust = count_rust(&functions);

    println!("|rs|py|cpp|Function Name|");
    println!("|-|-|-|--------------|");
    let print_emoji = |x, e| print!("{}|", if x > 0 { e } else { '🚧' });
    for (func_i, func) in functions.iter().enumerate() {
        print!("|");
        print_emoji(rust[func_i], '🦀');
        print_emoji(python[func_i], '🐍');
        print_emoji(c[func_i], '🐀');
        println!("{func}|");
    }
}

fn get_function_coreapi() -> Vec<String> {
    let clang = Clang::new().expect("Failed to load libclang");
    let index = Index::new(&clang, false, false);

    // parse the core api binaryninjacore.h
    let core_api = index
        .parser("../../binaryninjacore.h")
        .arguments(&["-xc++", "-D\"_cplusplus\""])
        .parse()
        .expect("Failed to parse");
    // get the functions
    let mut functions: Vec<String> = vec![];
    core_api.get_entity().visit_children(|entity, _parent| {
        if entity.get_kind() == EntityKind::FunctionDecl {
            if let Some(name) = entity.get_name() {
                functions.push(name.clone());
            }
        }
        clang::EntityVisitResult::Recurse
    });
    functions
}

fn count_c(functions_order: &[String]) -> Vec<usize> {
    let build_dir = "../../build/compile_commands.json";

    let json = std::fs::read_to_string(&build_dir).expect(
        r#"Failed to load compile_commands.json, please compile binaryninja-api using\n
            `cmake -DCMAKE_EXPORT_COMPILE_COMMANDS=ON -B build -S .`"#,
    );

    #[derive(Debug, Clone, Deserialize)]
    struct Command {
        #[serde(rename = "directory")]
        _directory: String,
        command: String,
        file: String,
        #[serde(rename = "output")]
        _output: String,
    }
    let commands: Vec<(String, Vec<String>)> = serde_json::from_str::<Vec<Command>>(&json)
        .expect("Unexpected `compile_commands.json` contents")
        .into_iter()
        .map(|x| {
            let mut args = vec![];
            let mut command_iter = x.command.split_whitespace();
            if let Some(command_raw) = command_iter.next() {
                let command = Path::new(command_raw).file_name();
                match command.and_then(OsStr::to_str) {
                    Some("g++" | "clang++") => args.push("-xc++".into()),
                    Some("gcc" | "clang") => args.push("-xc".into()),
                    _ => {}
                }
            }

            loop {
                let Some(arg) = command_iter.next() else {
                    break;
                };
                match arg {
                    // remove the filename from the arguments
                    file if file == x.file => {}
                    // remove the compile flag
                    "-c" => {}
                    // remove the output file name
                    "-o" => {
                        let _filename = command_iter.next();
                    }
                    // add the other args
                    _ => args.push(arg.into()),
                }
            }

            (x.file, args)
        })
        .collect();

    let mut functions: HashMap<String, usize> =
        functions_order.iter().map(|x| (x.to_string(), 0)).collect();

    let clang = Clang::new().expect("Failed to load libclang");
    let index = Index::new(&clang, false, false);

    // Iterate over all compile commands
    for (filename, args) in commands {
        // parse the file
        let parsed = index
            .parser(&filename)
            .arguments(&args)
            .parse()
            .expect("Failed to parse");

        // check if the file use functions from coreapi
        parsed.get_entity().visit_children(|entity, _parent| {
            // only function calls, resolved or not
            if matches!(
                entity.get_kind(),
                EntityKind::CallExpr | EntityKind::OverloadedDeclRef
            ) {
                if let Some(name) = entity.get_name() {
                    functions.entry(name).and_modify(|x| *x += 1);
                }
            }
            clang::EntityVisitResult::Recurse
        });
    }

    function_in_order(functions_order, functions)
}

// TODO parse the python coreapi, don't reuse the c one
fn count_python(functions_order: &[String]) -> Vec<usize> {
    let mut functions: HashMap<String, usize> =
        functions_order.iter().map(|x| (x.to_string(), 0)).collect();

    // check all file inside python, get all .py files
    let files = get_all_files(Path::new("../../python"), "py");

    use pyo3::prelude::*;
    Python::attach(|py| -> PyResult<()> {
        let ast = PyModule::import(py, "ast")?;
        let ast_call = ast.getattr("Call")?;
        let ast_name = ast.getattr("Name")?;
        let ast_attr = ast.getattr("Attribute")?;
        let isinstance = py.eval(c"isinstance", None, None)?;
        for file in files {
            let data = std::fs::read(file).expect("Unable to read python file");
            let tree = ast.call_method1("parse", (data,))?;
            let walk = ast.call_method1("walk", (&tree,))?;
            for node in walk.try_iter()? {
                let node = node?;
                let is_call: bool = isinstance.call((&node, &ast_call), None)?.extract()?;
                if is_call {
                    let func = node.getattr("func")?;
                    let func_name = if isinstance
                        .call((&func, &ast_name), None)?
                        .extract::<bool>()?
                    {
                        Some(func.getattr("id")?.extract::<String>()?)
                    } else if isinstance
                        .call((&func, &ast_attr), None)?
                        .extract::<bool>()?
                    {
                        Some(func.getattr("attr")?.extract::<String>()?)
                    } else {
                        None
                    };

                    if let Some(func_name) = func_name {
                        functions.entry(func_name).and_modify(|x| *x += 1);
                    }
                }
            }
        }
        Ok(())
    })
    .expect("Unable to execute python");

    function_in_order(functions_order, functions)
}

fn count_rust(functions_order: &[String]) -> Vec<usize> {
    // visitor logic
    struct CallCounter {
        functions: HashMap<String, usize>,
    }
    impl<'ast> Visit<'ast> for CallCounter {
        fn visit_expr_call(&mut self, node: &'ast ExprCall) {
            if let syn::Expr::Path(ref path) = *node.func {
                if let Some(ident) = path.path.get_ident() {
                    self.functions
                        .entry(ident.to_string())
                        .and_modify(|x| *x += 1);
                }
            }
            syn::visit::visit_expr_call(self, node);
        }
    }
    let mut counter = CallCounter {
        functions: functions_order.iter().map(|x| (x.to_string(), 0)).collect(),
    };

    // parse and check all the .rs files
    for file in get_all_files(Path::new("../../rust"), "rs") {
        let file_content = std::fs::read_to_string(file).expect("Unable to read rust file");

        let syn = syn::parse_file(&file_content).expect("Unable to parse rust file");

        // visit rust file
        counter.visit_file(&syn);
    }

    function_in_order(functions_order, counter.functions)
}

fn function_in_order(functions_order: &[String], functions: HashMap<String, usize>) -> Vec<usize> {
    functions_order
        .into_iter()
        .map(|func| functions[func.as_str()])
        .collect()
}

fn get_all_files(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut files = vec![];
    let mut directories = vec![dir.to_owned()];
    loop {
        let Some(dir) = directories.pop() else {
            break;
        };

        for entry in std::fs::read_dir(dir).expect("Unable to find the python dir") {
            let entry = entry.expect("invalid entry in the python directory");
            let ftype = entry
                .file_type()
                .expect("Unable to identify file type in python directory");
            if ftype.is_dir() {
                // check the directory after this one
                directories.push(entry.path());
            } else if ftype.is_file() || ftype.is_symlink() {
                // if a python file, check it
                if Path::new(&entry.file_name()).extension() == Some(OsStr::new(ext)) {
                    files.push(entry.path());
                }
            }
        }
    }
    files
}
