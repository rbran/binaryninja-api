// Copyright 2021-2024 Vector 35 Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use binaryninjacore_sys::*;
use std::io::{Read, Seek, SeekFrom, Write};
use std::marker::PhantomData;
use std::path::PathBuf;
use std::slice;

use crate::databuffer::DataBuffer;
use crate::string::BnString;

pub struct FileAccessor<'a> {
    pub(crate) api_object: BNFileAccessor,
    _ref: PhantomData<&'a mut ()>,
}

impl<'a> FileAccessor<'a> {
    pub fn new<F>(f: &'a mut F) -> Self
    where
        F: 'a + Read + Write + Seek + Sized,
    {
        use std::os::raw::c_void;

        extern "C" fn cb_get_length<F>(ctxt: *mut c_void) -> u64
        where
            F: Read + Write + Seek + Sized,
        {
            let f = unsafe { &mut *(ctxt as *mut F) };

            f.seek(SeekFrom::End(0)).unwrap_or(0)
        }

        extern "C" fn cb_read<F>(
            ctxt: *mut c_void,
            dest: *mut c_void,
            offset: u64,
            len: usize,
        ) -> usize
        where
            F: Read + Write + Seek + Sized,
        {
            let f = unsafe { &mut *(ctxt as *mut F) };
            let dest = unsafe { slice::from_raw_parts_mut(dest as *mut u8, len) };

            if f.seek(SeekFrom::Start(offset)).is_err() {
                debug!("Failed to seek to offset {:x}", offset);
                0
            } else {
                f.read(dest).unwrap_or(0)
            }
        }

        extern "C" fn cb_write<F>(
            ctxt: *mut c_void,
            offset: u64,
            src: *const c_void,
            len: usize,
        ) -> usize
        where
            F: Read + Write + Seek + Sized,
        {
            let f = unsafe { &mut *(ctxt as *mut F) };
            let src = unsafe { slice::from_raw_parts(src as *const u8, len) };

            if f.seek(SeekFrom::Start(offset)).is_err() {
                0
            } else {
                f.write(src).unwrap_or(0)
            }
        }

        Self {
            api_object: BNFileAccessor {
                context: f as *mut F as *mut _,
                getLength: Some(cb_get_length::<F>),
                read: Some(cb_read::<F>),
                write: Some(cb_write::<F>),
            },
            _ref: PhantomData,
        }
    }
}

pub struct TemporaryFile(*mut BNTemporaryFile);

impl TemporaryFile {
    pub fn new_from_contents(value: &DataBuffer) -> Result<Self, ()> {
        let new = unsafe { BNCreateTemporaryFileWithContents(value.as_raw()) };
        if new.is_null() {
            return Err(());
        }
        Ok(Self(new))
    }

    /// create a new reference to the same file
    pub fn clone_reference(&self) -> Self {
        let new = unsafe { BNNewTemporaryFileReference(self.0) };
        assert!(!new.is_null());
        Self(new)
    }

    pub fn path(&self) -> PathBuf {
        let path = unsafe { BnString::from_raw(BNGetTemporaryFilePath(self.0)) };
        PathBuf::from(path.to_string())
    }

    // TODO is databuffer lifetime associated with the TemporaryFile?
    // can we modify the DataBuffer without affecting the temp file?
    pub fn contents(&self) -> DataBuffer {
        unsafe { DataBuffer::from_raw(BNGetTemporaryFileContents(self.0)) }
    }
}

impl Default for TemporaryFile {
    fn default() -> Self {
        Self(unsafe { BNCreateTemporaryFile() })
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        unsafe { BNFreeTemporaryFile(self.0) }
    }
}

#[cfg(test)]
mod test {
    use crate::databuffer::DataBuffer;

    use super::TemporaryFile;

    #[test]
    pub fn create_tmp_file() {
        const DATA: &[u8] = b"test 123";
        let data = DataBuffer::new(DATA).unwrap();
        let tmp_file = TemporaryFile::new_from_contents(&data).unwrap();
        let data_read = tmp_file.contents();
        assert_eq!(data_read.get_data(), DATA);
    }
}
