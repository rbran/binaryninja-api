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

// TODO : More widely enforce the use of ref_from_raw vs just from_raw to simplify internal binding usage?  Perhaps remove from_raw functions?
// TODO : Add documentation and fix examples
// TODO : Test the get_enumeration and get_structure methods

use binaryninjacore_sys::*;

use crate::{
    architecture::{Architecture, CoreArchitecture},
    binaryview::{BinaryView, BinaryViewExt},
    callingconvention::CallingConvention,
    filemetadata::FileMetadata,
    function::Function,
    rc::*,
    string::{raw_to_string, BnStrCompatible, BnString},
    symbol::Symbol,
};

use lazy_static::lazy_static;
use std::{
    borrow::Cow,
    collections::HashSet,
    ffi::CStr,
    fmt,
    fmt::{Debug, Display, Formatter},
    hash::{Hash, Hasher},
    iter::IntoIterator,
    mem,
    os::raw::c_char,
    ptr, result, slice,
    sync::Mutex,
};

pub type Result<R> = result::Result<R, ()>;

pub type ReferenceType = BNReferenceType;
pub type TypeClass = BNTypeClass;
pub type NamedTypeReferenceClass = BNNamedTypeReferenceClass;
pub type MemberAccess = BNMemberAccess;
pub type MemberScope = BNMemberScope;

////////////////
// Confidence

/// Compatible with the `BNType*WithConfidence` types
pub struct Conf<T> {
    pub contents: T,
    pub confidence: u8,
}

pub trait ConfMergable<T, O> {
    type Result;
    /// Merge two confidence types' values depending on whichever has higher confidence
    /// In the event of a tie, the LHS (caller's) value is used.
    fn merge(self, other: O) -> Self::Result;
}

impl<T> Conf<T> {
    pub fn new(contents: T, confidence: u8) -> Self {
        Self {
            contents,
            confidence,
        }
    }

    pub fn map<U, F>(self, f: F) -> Conf<U>
    where
        F: FnOnce(T) -> U,
    {
        Conf::new(f(self.contents), self.confidence)
    }

    pub fn as_ref(&self) -> Conf<&T> {
        Conf::new(&self.contents, self.confidence)
    }
}

/// Returns best value or LHS on tie
///
/// `Conf<T>` + `Conf<T>` → `Conf<T>`
impl<T> ConfMergable<T, Conf<T>> for Conf<T> {
    type Result = Conf<T>;
    fn merge(self, other: Conf<T>) -> Conf<T> {
        if other.confidence > self.confidence {
            other
        } else {
            self
        }
    }
}

/// Returns LHS if RHS is None
///
/// `Conf<T>` + `Option<Conf<T>>` → `Conf<T>`
impl<T> ConfMergable<T, Option<Conf<T>>> for Conf<T> {
    type Result = Conf<T>;
    fn merge(self, other: Option<Conf<T>>) -> Conf<T> {
        match other {
            Some(c @ Conf { confidence, .. }) if confidence > self.confidence => c,
            _ => self,
        }
    }
}

/// Returns RHS if LHS is None
///
/// `Option<Conf<T>>` + `Conf<T>` → `Conf<T>`
impl<T> ConfMergable<T, Conf<T>> for Option<Conf<T>> {
    type Result = Conf<T>;
    fn merge(self, other: Conf<T>) -> Conf<T> {
        match self {
            Some(c @ Conf { confidence, .. }) if confidence >= other.confidence => c,
            _ => other,
        }
    }
}

/// Returns best non-None value or None
///
/// `Option<Conf<T>>` + `Option<Conf<T>>` → `Option<Conf<T>>`
impl<T> ConfMergable<T, Option<Conf<T>>> for Option<Conf<T>> {
    type Result = Option<Conf<T>>;
    fn merge(self, other: Option<Conf<T>>) -> Option<Conf<T>> {
        match (self, other) {
            (
                Some(
                    this @ Conf {
                        confidence: this_confidence,
                        ..
                    },
                ),
                Some(
                    other @ Conf {
                        confidence: other_confidence,
                        ..
                    },
                ),
            ) => {
                if this_confidence >= other_confidence {
                    Some(this)
                } else {
                    Some(other)
                }
            }
            (None, Some(c)) => Some(c),
            (Some(c), None) => Some(c),
            (None, None) => None,
        }
    }
}

impl<T: Debug> Debug for Conf<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} ({} confidence)", self.contents, self.confidence)
    }
}

impl<T: Display> Display for Conf<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({} confidence)", self.contents, self.confidence)
    }
}

#[inline]
pub fn min_confidence() -> u8 {
    u8::MIN
}

#[inline]
pub fn max_confidence() -> u8 {
    u8::MAX
}

impl<T: Clone> Clone for Conf<T> {
    fn clone(&self) -> Self {
        Self {
            contents: self.contents.clone(),
            confidence: self.confidence,
        }
    }
}

impl<T> From<T> for Conf<T> {
    fn from(contents: T) -> Self {
        Self::new(contents, max_confidence())
    }
}

impl<'a, T> From<&'a Conf<T>> for Conf<&'a T> {
    fn from(value: &Conf<T>) -> Conf<&T> {
        value.as_ref()
    }
}

impl From<BNTypeWithConfidence> for Conf<Type> {
    fn from(type_with_confidence: BNTypeWithConfidence) -> Self {
        Self::new(
            unsafe { Type::from_raw(type_with_confidence.type_) },
            type_with_confidence.confidence,
        )
    }
}

impl From<BNBoolWithConfidence> for Conf<bool> {
    fn from(bool_with_confidence: BNBoolWithConfidence) -> Self {
        Self::new(bool_with_confidence.value, bool_with_confidence.confidence)
    }
}

impl From<BNCallingConventionWithConfidence> for Conf<CallingConvention<CoreArchitecture>> {
    fn from(cc_with_confidence: BNCallingConventionWithConfidence) -> Self {
        Self::new(
            unsafe {
                CallingConvention::ref_from_raw(
                    cc_with_confidence.convention,
                    CoreArchitecture::from_raw(BNGetCallingConventionArchitecture(
                        cc_with_confidence.convention,
                    )),
                )
            },
            cc_with_confidence.confidence,
        )
    }
}

impl From<BNOffsetWithConfidence> for Conf<i64> {
    fn from(offset_with_confidence: BNOffsetWithConfidence) -> Self {
        Self::new(
            offset_with_confidence.value,
            offset_with_confidence.confidence,
        )
    }
}

impl From<Conf<&Type>> for BNTypeWithConfidence {
    fn from(conf: Conf<&Type>) -> Self {
        Self {
            type_: conf.contents.handle,
            confidence: conf.confidence,
        }
    }
}

impl From<Conf<bool>> for BNBoolWithConfidence {
    fn from(conf: Conf<bool>) -> Self {
        Self {
            value: conf.contents,
            confidence: conf.confidence,
        }
    }
}

impl<A: Architecture> From<Conf<&CallingConvention<A>>> for BNCallingConventionWithConfidence {
    fn from(conf: Conf<&CallingConvention<A>>) -> Self {
        Self {
            convention: conf.contents.handle,
            confidence: conf.confidence,
        }
    }
}

impl From<Conf<i64>> for BNOffsetWithConfidence {
    fn from(conf: Conf<i64>) -> Self {
        Self {
            value: conf.contents,
            confidence: conf.confidence,
        }
    }
}

//////////////////
// Type Builder

#[repr(transparent)]
#[derive(PartialEq, Eq, Hash)]
pub struct TypeBuilder {
    pub(crate) handle: *mut BNTypeBuilder,
}

impl TypeBuilder {
    pub fn new(t: &Type) -> Self {
        unsafe { Self::from_raw(BNCreateTypeBuilderFromType(t.handle)) }
    }

    pub(crate) unsafe fn from_raw(handle: *mut BNTypeBuilder) -> Self {
        debug_assert!(!handle.is_null());
        Self { handle }
    }

    // Chainable terminal
    pub fn finalize(&self) -> Type {
        unsafe { Type::from_raw(BNFinalizeTypeBuilder(self.handle)) }
    }

    // Settable properties

    pub fn set_can_return<T: Into<Conf<bool>>>(&self, value: T) -> &Self {
        let mut bool_with_confidence = value.into().into();
        unsafe { BNSetFunctionTypeBuilderCanReturn(self.handle, &mut bool_with_confidence) };
        self
    }

    pub fn set_pure<T: Into<Conf<bool>>>(&self, value: T) -> &Self {
        let mut bool_with_confidence = value.into().into();
        unsafe { BNSetTypeBuilderPure(self.handle, &mut bool_with_confidence) };
        self
    }

    pub fn set_const<T: Into<Conf<bool>>>(&self, value: T) -> &Self {
        let mut bool_with_confidence = value.into().into();
        unsafe { BNTypeBuilderSetConst(self.handle, &mut bool_with_confidence) };
        self
    }

    pub fn set_volatile<T: Into<Conf<bool>>>(&self, value: T) -> &Self {
        let mut bool_with_confidence = value.into().into();
        unsafe { BNTypeBuilderSetVolatile(self.handle, &mut bool_with_confidence) };
        self
    }

    // Readable properties

    pub fn type_class(&self) -> TypeClass {
        unsafe { BNGetTypeBuilderClass(self.handle) }
    }

    pub fn width(&self) -> u64 {
        unsafe { BNGetTypeBuilderWidth(self.handle) }
    }

    pub fn alignment(&self) -> usize {
        unsafe { BNGetTypeBuilderAlignment(self.handle) }
    }

    pub fn is_signed(&self) -> Conf<bool> {
        unsafe { BNIsTypeBuilderSigned(self.handle).into() }
    }

    pub fn is_const(&self) -> Conf<bool> {
        unsafe { BNIsTypeBuilderConst(self.handle).into() }
    }

    pub fn is_volatile(&self) -> Conf<bool> {
        unsafe { BNIsTypeBuilderVolatile(self.handle).into() }
    }

    pub fn is_floating_point(&self) -> bool {
        unsafe { BNIsTypeBuilderFloatingPoint(self.handle) }
    }

    pub fn target(&self) -> Result<Conf<Type>> {
        let raw_target = unsafe { BNGetTypeBuilderChildType(self.handle) };
        if raw_target.type_.is_null() {
            Err(())
        } else {
            Ok(raw_target.into())
        }
    }

    pub fn element_type(&self) -> Result<Conf<Type>> {
        let raw_target = unsafe { BNGetTypeBuilderChildType(self.handle) };
        if raw_target.type_.is_null() {
            Err(())
        } else {
            Ok(raw_target.into())
        }
    }

    pub fn return_value(&self) -> Result<Conf<Type>> {
        let raw_target = unsafe { BNGetTypeBuilderChildType(self.handle) };
        if raw_target.type_.is_null() {
            Err(())
        } else {
            Ok(raw_target.into())
        }
    }

    pub fn calling_convention(&self) -> Result<Conf<CallingConvention<CoreArchitecture>>> {
        let convention_confidence = unsafe { BNGetTypeBuilderCallingConvention(self.handle) };
        if convention_confidence.convention.is_null() {
            Err(())
        } else {
            Ok(convention_confidence.into())
        }
    }

    pub fn parameters(&self) -> Result<Vec<FunctionParameter>> {
        unsafe {
            let mut count = 0;
            let parameters_raw = BNGetTypeBuilderParameters(self.handle, &mut count);
            if parameters_raw.is_null() {
                Err(())
            } else {
                let parameters: &[BNFunctionParameter] =
                    slice::from_raw_parts(parameters_raw, count);

                let result = (0..count)
                    .map(|i| FunctionParameter::from_raw(parameters[i]))
                    .collect();

                BNFreeTypeParameterList(parameters_raw, count);

                Ok(result)
            }
        }
    }

    pub fn has_variable_arguments(&self) -> Conf<bool> {
        unsafe { BNTypeBuilderHasVariableArguments(self.handle).into() }
    }

    pub fn can_return(&self) -> Conf<bool> {
        unsafe { BNFunctionTypeBuilderCanReturn(self.handle).into() }
    }

    pub fn pure(&self) -> Conf<bool> {
        unsafe { BNIsTypeBuilderPure(self.handle).into() }
    }

    pub fn get_structure(&self) -> Result<Structure> {
        let result = unsafe { BNGetTypeBuilderStructure(self.handle) };
        if result.is_null() {
            Err(())
        } else {
            Ok(unsafe { Structure::from_raw(result) })
        }
    }

    pub fn get_enumeration(&self) -> Result<Enumeration> {
        let result = unsafe { BNGetTypeBuilderEnumeration(self.handle) };
        if result.is_null() {
            Err(())
        } else {
            Ok(unsafe { Enumeration::from_raw(result) })
        }
    }

    pub fn get_named_type_reference(&self) -> Result<NamedTypeReference> {
        let result = unsafe { BNGetTypeBuilderNamedTypeReference(self.handle) };
        if result.is_null() {
            Err(())
        } else {
            Ok(unsafe { NamedTypeReference::from_raw(result) })
        }
    }

    pub fn count(&self) -> u64 {
        unsafe { BNGetTypeBuilderElementCount(self.handle) }
    }

    pub fn offset(&self) -> u64 {
        unsafe { BNGetTypeBuilderOffset(self.handle) }
    }

    pub fn stack_adjustment(&self) -> Conf<i64> {
        unsafe { BNGetTypeBuilderStackAdjustment(self.handle).into() }
    }

    // TODO : This and properties
    // pub fn tokens(&self) -> ? {}

    pub fn void() -> Self {
        unsafe { Self::from_raw(BNCreateVoidTypeBuilder()) }
    }

    pub fn bool() -> Self {
        unsafe { Self::from_raw(BNCreateBoolTypeBuilder()) }
    }

    pub fn char() -> Self {
        Self::int(1, true)
    }

    pub fn int(width: usize, is_signed: bool) -> Self {
        let mut is_signed = Conf::new(is_signed, max_confidence()).into();

        unsafe {
            Self::from_raw(BNCreateIntegerTypeBuilder(
                width,
                &mut is_signed,
                BnString::new("").as_ptr() as *mut _,
            ))
        }
    }

    pub fn named_int<S: BnStrCompatible>(width: usize, is_signed: bool, alt_name: S) -> Self {
        let mut is_signed = Conf::new(is_signed, max_confidence()).into();
        // let alt_name = BnString::new(alt_name);
        let alt_name = alt_name.into_bytes_with_nul(); // This segfaulted once, so the above version is there if we need to change to it, but in theory this is copied into a `const string&` on the C++ side; I'm just not 100% confident that a constant reference copies data

        unsafe {
            Self::from_raw(BNCreateIntegerTypeBuilder(
                width,
                &mut is_signed,
                alt_name.as_ref().as_ptr() as _,
            ))
        }
    }

    pub fn float(width: usize) -> Self {
        unsafe {
            Self::from_raw(BNCreateFloatTypeBuilder(
                width,
                BnString::new("").as_ptr() as *mut _,
            ))
        }
    }

    pub fn named_float<S: BnStrCompatible>(width: usize, alt_name: S) -> Self {
        // let alt_name = BnString::new(alt_name);
        let alt_name = alt_name.into_bytes_with_nul(); // See same line in `named_int` above

        unsafe {
            Self::from_raw(BNCreateFloatTypeBuilder(
                width,
                alt_name.as_ref().as_ptr() as _,
            ))
        }
    }

    pub fn array<'a, T: Into<Conf<&'a Type>>>(t: T, count: u64) -> Self {
        unsafe { Self::from_raw(BNCreateArrayTypeBuilder(&t.into().into(), count)) }
    }

    /// The C/C++ APIs require an associated architecture, but in the core we only query the default_int_size if the given width is 0
    /// For simplicity's sake, that convention isn't followed and you can query the default_int_size from an arch, if you have it, if you need to
    pub fn enumeration<T: Into<Conf<bool>>>(
        enumeration: &Enumeration,
        width: usize,
        is_signed: T,
    ) -> Self {
        unsafe {
            // TODO : This is _extremely fragile_, we should change the internals of BNCreateEnumerationTypeBuilder instead of doing this
            let mut fake_arch: BNArchitecture = mem::zeroed();
            Self::from_raw(BNCreateEnumerationTypeBuilder(
                &mut fake_arch,
                enumeration.handle,
                width,
                &mut is_signed.into().into(),
            ))
        }
    }

    pub fn structure(structure_type: &Structure) -> Self {
        unsafe { Self::from_raw(BNCreateStructureTypeBuilder(structure_type.handle)) }
    }

    pub fn named_type(type_reference: NamedTypeReference) -> Self {
        let mut is_const = Conf::new(false, min_confidence()).into();
        let mut is_volatile = Conf::new(false, min_confidence()).into();
        unsafe {
            Self::from_raw(BNCreateNamedTypeReferenceBuilder(
                type_reference.handle,
                0,
                1,
                &mut is_const,
                &mut is_volatile,
            ))
        }
    }

    pub fn named_type_from_type<S: BnStrCompatible>(name: S, t: &Type) -> Self {
        let mut name = QualifiedName::from(name);

        unsafe {
            Self::from_raw(BNCreateNamedTypeReferenceBuilderFromTypeAndId(
                BnString::new("").as_ptr() as *mut _,
                &mut name.0,
                t.handle,
            ))
        }
    }

    // TODO : BNCreateFunctionTypeBuilder

    pub fn pointer<'a, A: Architecture, T: Into<Conf<&'a Type>>>(arch: &A, t: T) -> Self {
        let mut is_const = Conf::new(false, min_confidence()).into();
        let mut is_volatile = Conf::new(false, min_confidence()).into();

        unsafe {
            Self::from_raw(BNCreatePointerTypeBuilder(
                arch.as_ref().0,
                &t.into().into(),
                &mut is_const,
                &mut is_volatile,
                ReferenceType::PointerReferenceType,
            ))
        }
    }

    pub fn const_pointer<'a, A: Architecture, T: Into<Conf<&'a Type>>>(arch: &A, t: T) -> Self {
        let mut is_const = Conf::new(true, max_confidence()).into();
        let mut is_volatile = Conf::new(false, min_confidence()).into();

        unsafe {
            Self::from_raw(BNCreatePointerTypeBuilder(
                arch.as_ref().0,
                &t.into().into(),
                &mut is_const,
                &mut is_volatile,
                ReferenceType::PointerReferenceType,
            ))
        }
    }

    pub fn pointer_of_width<'a, T: Into<Conf<&'a Type>>>(
        t: T,
        size: usize,
        is_const: bool,
        is_volatile: bool,
        ref_type: Option<ReferenceType>,
    ) -> Self {
        let mut is_const = Conf::new(is_const, max_confidence()).into();
        let mut is_volatile = Conf::new(is_volatile, max_confidence()).into();

        unsafe {
            Self::from_raw(BNCreatePointerTypeBuilderOfWidth(
                size,
                &t.into().into(),
                &mut is_const,
                &mut is_volatile,
                ref_type.unwrap_or(ReferenceType::PointerReferenceType),
            ))
        }
    }

    pub fn pointer_with_options<'a, A: Architecture, T: Into<Conf<&'a Type>>>(
        arch: &A,
        t: T,
        is_const: bool,
        is_volatile: bool,
        ref_type: Option<ReferenceType>,
    ) -> Self {
        let mut is_const = Conf::new(is_const, max_confidence()).into();
        let mut is_volatile = Conf::new(is_volatile, max_confidence()).into();
        unsafe {
            Self::from_raw(BNCreatePointerTypeBuilder(
                arch.as_ref().0,
                &t.into().into(),
                &mut is_const,
                &mut is_volatile,
                ref_type.unwrap_or(ReferenceType::PointerReferenceType),
            ))
        }
    }
}

impl fmt::Display for TypeBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", unsafe {
            BnString::from_raw(BNGetTypeBuilderString(self.handle, ptr::null_mut()))
        })
    }
}

impl Drop for TypeBuilder {
    fn drop(&mut self) {
        unsafe { BNFreeTypeBuilder(self.handle) };
    }
}

//////////
// Type

#[repr(transparent)]
pub struct Type {
    pub(crate) handle: *mut BNType,
}

/// ```no_run
/// # use crate::binaryninja::binaryview::BinaryViewExt;
/// # use binaryninja::types::Type;
/// let bv = binaryninja::load("example.bin").unwrap();
/// let my_custom_type_1 = Type::named_int(5, false, "my_w");
/// let my_custom_type_2 = Type::int(5, false);
/// bv.define_user_type("int_1", &my_custom_type_1);
/// bv.define_user_type("int_2", &my_custom_type_2);
/// ```
impl Type {
    pub(crate) unsafe fn from_raw(handle: *mut BNType) -> Self {
        debug_assert!(!handle.is_null());
        Self { handle }
    }

    pub fn to_builder(&self) -> TypeBuilder {
        TypeBuilder::new(self)
    }

    // Readable properties

    pub fn type_class(&self) -> TypeClass {
        unsafe { BNGetTypeClass(self.handle) }
    }

    pub fn width(&self) -> u64 {
        unsafe { BNGetTypeWidth(self.handle) }
    }

    pub fn alignment(&self) -> usize {
        unsafe { BNGetTypeAlignment(self.handle) }
    }

    pub fn is_signed(&self) -> Conf<bool> {
        unsafe { BNIsTypeSigned(self.handle).into() }
    }

    pub fn is_const(&self) -> Conf<bool> {
        unsafe { BNIsTypeConst(self.handle).into() }
    }

    pub fn is_volatile(&self) -> Conf<bool> {
        unsafe { BNIsTypeVolatile(self.handle).into() }
    }

    pub fn is_floating_point(&self) -> bool {
        unsafe { BNIsTypeFloatingPoint(self.handle) }
    }

    pub fn target(&self) -> Result<Conf<Type>> {
        let raw_target = unsafe { BNGetChildType(self.handle) };
        if raw_target.type_.is_null() {
            Err(())
        } else {
            Ok(raw_target.into())
        }
    }

    pub fn element_type(&self) -> Result<Conf<Type>> {
        let raw_target = unsafe { BNGetChildType(self.handle) };
        if raw_target.type_.is_null() {
            Err(())
        } else {
            Ok(raw_target.into())
        }
    }

    pub fn return_value(&self) -> Result<Conf<Type>> {
        let raw_target = unsafe { BNGetChildType(self.handle) };
        if raw_target.type_.is_null() {
            Err(())
        } else {
            Ok(raw_target.into())
        }
    }

    pub fn calling_convention(&self) -> Result<Conf<CallingConvention<CoreArchitecture>>> {
        let convention_confidence = unsafe { BNGetTypeCallingConvention(self.handle) };
        if convention_confidence.convention.is_null() {
            Err(())
        } else {
            Ok(convention_confidence.into())
        }
    }

    pub fn parameters(&self) -> Result<Vec<FunctionParameter>> {
        unsafe {
            let mut count = 0;
            let parameters_raw: *mut BNFunctionParameter =
                BNGetTypeParameters(self.handle, &mut count);
            if parameters_raw.is_null() {
                Err(())
            } else {
                let parameters: &[BNFunctionParameter] =
                    slice::from_raw_parts(parameters_raw, count);

                let result = (0..count)
                    .map(|i| FunctionParameter::from_raw(parameters[i]))
                    .collect();

                BNFreeTypeParameterList(parameters_raw, count);

                Ok(result)
            }
        }
    }

    pub fn has_variable_arguments(&self) -> Conf<bool> {
        unsafe { BNTypeHasVariableArguments(self.handle).into() }
    }

    pub fn can_return(&self) -> Conf<bool> {
        unsafe { BNFunctionTypeCanReturn(self.handle).into() }
    }

    pub fn pure(&self) -> Conf<bool> {
        unsafe { BNIsTypePure(self.handle).into() }
    }

    pub fn get_structure(&self) -> Result<Structure> {
        let result = unsafe { BNGetTypeStructure(self.handle) };
        if result.is_null() {
            Err(())
        } else {
            Ok(unsafe { Structure::from_raw(result) })
        }
    }

    pub fn get_enumeration(&self) -> Result<Enumeration> {
        let result = unsafe { BNGetTypeEnumeration(self.handle) };
        if result.is_null() {
            Err(())
        } else {
            Ok(unsafe { Enumeration::from_raw(result) })
        }
    }

    pub fn get_named_type_reference(&self) -> Result<NamedTypeReference> {
        let result = unsafe { BNGetTypeNamedTypeReference(self.handle) };
        if result.is_null() {
            Err(())
        } else {
            Ok(unsafe { NamedTypeReference::from_raw(result) })
        }
    }

    pub fn count(&self) -> u64 {
        unsafe { BNGetTypeElementCount(self.handle) }
    }

    pub fn offset(&self) -> u64 {
        unsafe { BNGetTypeOffset(self.handle) }
    }

    pub fn stack_adjustment(&self) -> Conf<i64> {
        unsafe { BNGetTypeStackAdjustment(self.handle).into() }
    }

    pub fn registered_name(&self) -> Result<NamedTypeReference> {
        let result = unsafe { BNGetRegisteredTypeName(self.handle) };
        if result.is_null() {
            Err(())
        } else {
            Ok(unsafe { NamedTypeReference::from_raw(result) })
        }
    }

    // TODO : This and properties
    // pub fn tokens(&self) -> ? {}

    pub fn void() -> Self {
        unsafe { Self::from_raw(BNCreateVoidType()) }
    }

    pub fn bool() -> Self {
        unsafe { Self::from_raw(BNCreateBoolType()) }
    }

    pub fn char() -> Self {
        Self::int(1, true)
    }

    pub fn wide_char(width: usize) -> Self {
        unsafe {
            Self::from_raw(BNCreateWideCharType(
                width,
                BnString::new("").as_ptr() as *mut _,
            ))
        }
    }

    pub fn int(width: usize, is_signed: bool) -> Self {
        let mut is_signed = Conf::new(is_signed, max_confidence()).into();
        unsafe {
            Self::from_raw(BNCreateIntegerType(
                width,
                &mut is_signed,
                BnString::new("").as_ptr() as *mut _,
            ))
        }
    }

    pub fn named_int<S: BnStrCompatible>(width: usize, is_signed: bool, alt_name: S) -> Self {
        let mut is_signed = Conf::new(is_signed, max_confidence()).into();
        // let alt_name = BnString::new(alt_name);
        let alt_name = alt_name.into_bytes_with_nul(); // This segfaulted once, so the above version is there if we need to change to it, but in theory this is copied into a `const string&` on the C++ side; I'm just not 100% confident that a constant reference copies data

        unsafe {
            Self::from_raw(BNCreateIntegerType(
                width,
                &mut is_signed,
                alt_name.as_ref().as_ptr() as _,
            ))
        }
    }

    pub fn float(width: usize) -> Self {
        unsafe {
            Self::from_raw(BNCreateFloatType(
                width,
                BnString::new("").as_ptr() as *mut _,
            ))
        }
    }

    pub fn named_float<S: BnStrCompatible>(width: usize, alt_name: S) -> Self {
        // let alt_name = BnString::new(alt_name);
        let alt_name = alt_name.into_bytes_with_nul(); // See same line in `named_int` above

        unsafe { Self::from_raw(BNCreateFloatType(width, alt_name.as_ref().as_ptr() as _)) }
    }

    pub fn array<'a, T: Into<Conf<&'a Type>>>(t: T, count: u64) -> Self {
        unsafe { Self::from_raw(BNCreateArrayType(&t.into().into(), count)) }
    }

    /// The C/C++ APIs require an associated architecture, but in the core we only query the default_int_size if the given width is 0
    ///
    /// For simplicity's sake, that convention isn't followed and you can query the default_int_size from an arch, if you have it, if you need to
    pub fn enumeration<T: Into<Conf<bool>>>(
        enumeration: &Enumeration,
        width: usize,
        is_signed: T,
    ) -> Self {
        unsafe {
            // TODO : This is _extremely fragile_, we should change the internals of BNCreateEnumerationType instead of doing this
            let mut fake_arch: BNArchitecture = mem::zeroed();
            Self::from_raw(BNCreateEnumerationType(
                &mut fake_arch,
                enumeration.handle,
                width,
                &mut is_signed.into().into(),
            ))
        }
    }

    pub fn structure(structure: &Structure) -> Self {
        unsafe { Self::from_raw(BNCreateStructureType(structure.handle)) }
    }

    pub fn named_type(type_reference: &NamedTypeReference) -> Self {
        let mut is_const = Conf::new(false, min_confidence()).into();
        let mut is_volatile = Conf::new(false, min_confidence()).into();
        unsafe {
            Self::from_raw(BNCreateNamedTypeReference(
                type_reference.handle,
                0,
                1,
                &mut is_const,
                &mut is_volatile,
            ))
        }
    }

    pub fn named_type_from_type<S: BnStrCompatible>(name: S, t: &Type) -> Self {
        let mut name = QualifiedName::from(name);

        unsafe {
            Self::from_raw(BNCreateNamedTypeReferenceFromTypeAndId(
                BnString::new("").as_ptr() as *mut _,
                &mut name.0,
                t.handle,
            ))
        }
    }

    pub fn function<'a, T: Into<Conf<&'a Type>>>(
        return_type: T,
        parameters: &[FunctionParameter],
        variable_arguments: bool,
    ) -> Self {
        let mut return_type = return_type.into().into();
        let mut variable_arguments = Conf::new(variable_arguments, max_confidence()).into();
        let mut can_return = Conf::new(true, min_confidence()).into();
        let mut pure = Conf::new(false, min_confidence()).into();

        let mut raw_calling_convention: BNCallingConventionWithConfidence =
            BNCallingConventionWithConfidence {
                convention: ptr::null_mut(),
                confidence: min_confidence(),
            };

        let mut stack_adjust = Conf::<i64>::new(0, min_confidence()).into();
        // NOTE this is safe because FunctionParameter and BNFunctionParameter
        // are transparent
        let raw_parameters: &[BNFunctionParameter] = unsafe { mem::transmute(parameters) };
        let reg_stack_adjust_regs = ptr::null_mut();
        let reg_stack_adjust_values = ptr::null_mut();

        let mut return_regs: BNRegisterSetWithConfidence = BNRegisterSetWithConfidence {
            regs: ptr::null_mut(),
            count: 0,
            confidence: 0,
        };

        unsafe {
            Self::from_raw(BNNewTypeReference(BNCreateFunctionType(
                &mut return_type,
                &mut raw_calling_convention,
                raw_parameters.as_ptr() as *mut BNFunctionParameter,
                raw_parameters.len(),
                &mut variable_arguments,
                &mut can_return,
                &mut stack_adjust,
                reg_stack_adjust_regs,
                reg_stack_adjust_values,
                0,
                &mut return_regs,
                BNNameType::NoNameType,
                &mut pure,
            )))
        }
    }

    pub fn function_with_options<
        'a,
        A: Architecture,
        T: Into<Conf<&'a Type>>,
        C: Into<Conf<&'a CallingConvention<A>>>,
    >(
        return_type: T,
        parameters: &[FunctionParameter],
        variable_arguments: bool,
        calling_convention: C,
        stack_adjust: Conf<i64>,
    ) -> Self {
        let mut return_type = return_type.into().into();
        let mut variable_arguments = Conf::new(variable_arguments, max_confidence()).into();
        let mut can_return = Conf::new(true, min_confidence()).into();
        let mut pure = Conf::new(false, min_confidence()).into();
        let mut raw_calling_convention: BNCallingConventionWithConfidence =
            calling_convention.into().into();
        let mut stack_adjust = stack_adjust.into();

        // NOTE this is safe because FunctionParameter and BNFunctionParameter
        // are transparent
        let raw_parameters: &[BNFunctionParameter] = unsafe { mem::transmute(parameters) };
        let mut name_ptrs = vec![];
        for parameter in parameters {
            name_ptrs.push(parameter.name().to_owned());
        }

        // TODO: Update type signature and include these (will be a breaking change)
        let reg_stack_adjust_regs = ptr::null_mut();
        let reg_stack_adjust_values = ptr::null_mut();

        let mut return_regs: BNRegisterSetWithConfidence = BNRegisterSetWithConfidence {
            regs: ptr::null_mut(),
            count: 0,
            confidence: 0,
        };

        unsafe {
            Self::from_raw(BNCreateFunctionType(
                &mut return_type,
                &mut raw_calling_convention,
                raw_parameters.as_ptr() as *mut BNFunctionParameter,
                raw_parameters.len(),
                &mut variable_arguments,
                &mut can_return,
                &mut stack_adjust,
                reg_stack_adjust_regs,
                reg_stack_adjust_values,
                0,
                &mut return_regs,
                BNNameType::NoNameType,
                &mut pure,
            ))
        }
    }

    pub fn pointer<'a, A: Architecture, T: Into<Conf<&'a Type>>>(arch: &A, t: T) -> Self {
        let mut is_const = Conf::new(false, min_confidence()).into();
        let mut is_volatile = Conf::new(false, min_confidence()).into();
        unsafe {
            Self::from_raw(BNCreatePointerType(
                arch.as_ref().0,
                &t.into().into(),
                &mut is_const,
                &mut is_volatile,
                ReferenceType::PointerReferenceType,
            ))
        }
    }

    pub fn const_pointer<'a, A: Architecture, T: Into<Conf<&'a Type>>>(arch: &A, t: T) -> Self {
        let mut is_const = Conf::new(true, max_confidence()).into();
        let mut is_volatile = Conf::new(false, min_confidence()).into();
        unsafe {
            Self::from_raw(BNCreatePointerType(
                arch.as_ref().0,
                &t.into().into(),
                &mut is_const,
                &mut is_volatile,
                ReferenceType::PointerReferenceType,
            ))
        }
    }

    pub fn pointer_of_width<'a, T: Into<Conf<&'a Type>>>(
        t: T,
        size: usize,
        is_const: bool,
        is_volatile: bool,
        ref_type: Option<ReferenceType>,
    ) -> Self {
        let mut is_const = Conf::new(is_const, max_confidence()).into();
        let mut is_volatile = Conf::new(is_volatile, max_confidence()).into();
        unsafe {
            Self::from_raw(BNCreatePointerTypeOfWidth(
                size,
                &t.into().into(),
                &mut is_const,
                &mut is_volatile,
                ref_type.unwrap_or(ReferenceType::PointerReferenceType),
            ))
        }
    }

    pub fn pointer_with_options<'a, A: Architecture, T: Into<Conf<&'a Type>>>(
        arch: &A,
        t: T,
        is_const: bool,
        is_volatile: bool,
        ref_type: Option<ReferenceType>,
    ) -> Self {
        let mut is_const = Conf::new(is_const, max_confidence()).into();
        let mut is_volatile = Conf::new(is_volatile, max_confidence()).into();
        unsafe {
            Self::from_raw(BNCreatePointerType(
                arch.as_ref().0,
                &t.into().into(),
                &mut is_const,
                &mut is_volatile,
                ref_type.unwrap_or(ReferenceType::PointerReferenceType),
            ))
        }
    }

    pub fn generate_auto_demangled_type_id<'a, S: BnStrCompatible>(name: S) -> &'a str {
        let mut name = QualifiedName::from(name);
        unsafe { CStr::from_ptr(BNGenerateAutoDemangledTypeId(&mut name.0)) }
            .to_str()
            .unwrap()
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", unsafe {
            BnString::from_raw(BNGetTypeString(
                self.handle,
                ptr::null_mut(),
                BNTokenEscapingType::NoTokenEscapingType,
            ))
        })
    }
}

lazy_static! {
    static ref TYPE_DEBUG_BV: Mutex<Option<BinaryView>> =
        Mutex::new(BinaryView::from_data(&FileMetadata::new(), &[]).ok());
}

impl fmt::Debug for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Ok(lock) = TYPE_DEBUG_BV.lock() {
            if let Some(bv) = &*lock {
                let mut count: usize = 0;
                let container = unsafe { BNGetAnalysisTypeContainer(bv.handle) };
                let lines: *mut BNTypeDefinitionLine = unsafe {
                    BNGetTypeLines(
                        self.handle,
                        container,
                        "\x00".as_ptr() as *const c_char,
                        64,
                        false,
                        BNTokenEscapingType::NoTokenEscapingType,
                        &mut count as *mut usize,
                    )
                };
                unsafe {
                    BNFreeTypeContainer(container);
                }

                if lines.is_null() {
                    return Err(fmt::Error);
                }

                let line_slice: &[BNTypeDefinitionLine] =
                    unsafe { slice::from_raw_parts(lines, count) };

                for (i, line) in line_slice.iter().enumerate() {
                    if i > 0 {
                        writeln!(f)?;
                    }

                    let tokens: &[BNInstructionTextToken] =
                        unsafe { slice::from_raw_parts(line.tokens, line.count) };

                    for token in tokens {
                        let text: *const c_char = token.text;
                        let str = unsafe { CStr::from_ptr(text) };
                        write!(f, "{}", str.to_string_lossy())?;
                    }
                }

                unsafe {
                    BNFreeTypeDefinitionLineList(lines, count);
                }
                return Ok(());
            }
        }
        Err(fmt::Error)
    }
}

impl PartialEq for Type {
    fn eq(&self, other: &Self) -> bool {
        unsafe { BNTypesEqual(self.handle, other.handle) }
    }
}

impl Eq for Type {}

impl Hash for Type {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.handle.hash(state);
    }
}

unsafe impl Send for Type {}
unsafe impl Sync for Type {}

impl Clone for Type {
    fn clone(&self) -> Self {
        unsafe { Self::from_raw(BNNewTypeReference(self.handle)) }
    }
}

impl Drop for Type {
    fn drop(&mut self) {
        unsafe {
            BNFreeType(self.handle);
        }
    }
}

///////////////////////
// FunctionParameter

#[derive(Debug, Clone)]
#[repr(C)]
pub struct FunctionParameter {
    // NOTE BnString is NonNull, so this is equivalent to a ptr with null being possible
    name: Option<BnString>,
    type_: Type,
    type_confidence: u8,
    default_location: bool,
    location: Variable,
}

impl FunctionParameter {
    pub fn new<S, T>(t: T, name: S, location: Option<Variable>) -> Self
    where
        S: BnStrCompatible,
        T: Into<Conf<Type>>,
    {
        let t: Conf<Type> = t.into();
        Self {
            name: Some(BnString::new(name)),
            type_: t.contents,
            type_confidence: t.confidence,
            default_location: location.is_none(),
            location: location.unwrap_or(unsafe { Variable::from_raw(Default::default()) }),
        }
    }

    pub(crate) unsafe fn from_raw(raw: BNFunctionParameter) -> Self {
        Self {
            name: (!raw.name.is_null()).then(|| BnString::from_raw(raw.name)),
            type_: Type::from_raw(raw.type_),
            type_confidence: raw.typeConfidence,
            default_location: raw.defaultLocation,
            location: Variable::from_raw(raw.location),
        }
    }

    pub fn t(&self) -> Conf<&Type> {
        Conf::new(&self.type_, self.type_confidence)
    }

    pub fn name(&self) -> Cow<str> {
        if let Some(name) = &self.name {
            name.as_str().into()
        } else {
            if self.location.t() == BNVariableSourceType::RegisterVariableSourceType {
                format!("reg_{}", self.location.storage()).into()
            } else if self.location.t() == BNVariableSourceType::StackVariableSourceType {
                format!("arg_{}", self.location.storage()).into()
            } else {
                "".into()
            }
        }
    }

    pub fn set_name<S: BnStrCompatible>(&mut self, name: S) {
        // drop the old name and set the new one
        self.name = Some(BnString::new(name));
    }

    pub fn location(&self) -> Option<Variable> {
        (!self.default_location).then_some(self.location)
    }

    pub fn set_location(&mut self, value: Option<Variable>) {
        self.default_location = value.is_none();
        if let Some(value) = value {
            self.location = value;
        }
    }
}

//////////////
// Variable

#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct Variable(BNVariable);

impl PartialEq for Variable {
    fn eq(&self, other: &Self) -> bool {
        self.0.storage == other.0.storage
            && self.0.index == other.0.index
            && self.0.type_ == other.0.type_
    }
}
impl Eq for Variable {}
impl Hash for Variable {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.type_.hash(state);
        self.0.index.hash(state);
        self.0.storage.hash(state);
    }
}

impl Variable {
    pub fn new(t: BNVariableSourceType, index: u32, storage: i64) -> Self {
        Self(BNVariable {
            type_: t,
            index,
            storage,
        })
    }

    pub(crate) unsafe fn from_raw(var: BNVariable) -> Self {
        Self(var)
    }

    pub(crate) fn as_raw(&self) -> &BNVariable {
        &self.0
    }

    pub(crate) fn raw(&self) -> BNVariable {
        *self.as_raw()
    }

    pub fn index(&self) -> u32 {
        self.0.index
    }

    pub fn storage(&self) -> i64 {
        self.0.storage
    }

    pub fn t(&self) -> BNVariableSourceType {
        self.0.type_
    }
}

//////////////
// SSAVariable

#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq)]
pub struct SSAVariable {
    pub variable: Variable,
    pub version: usize,
}

impl SSAVariable {
    pub fn new(variable: Variable, version: usize) -> Self {
        Self { variable, version }
    }
}

///////////////
// NamedVariable

#[derive(Debug, Clone)]
#[repr(C)]
pub struct NamedTypedVariable {
    var: Variable,
    type_: Type,
    name: BnString,
    auto_defined: bool,
    type_confidence: u8,
}

impl NamedTypedVariable {
    pub(crate) unsafe fn ref_from_raw(raw: &BNVariableNameAndType) -> &Self {
        core::mem::transmute(raw)
    }

    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    pub fn var(&self) -> Variable {
        self.var
    }

    pub fn auto_defined(&self) -> bool {
        self.auto_defined
    }

    pub fn type_confidence(&self) -> u8 {
        self.type_confidence
    }

    pub fn var_type(&self) -> &Type {
        &self.type_
    }
}

impl CoreArrayProvider for NamedTypedVariable {
    type Raw = BNVariableNameAndType;
    type Context = ();
    type Wrapped<'a> = &'a NamedTypedVariable;
}

unsafe impl CoreArrayProviderInner for NamedTypedVariable {
    unsafe fn free(raw: *mut Self::Raw, count: usize, _context: &Self::Context) {
        BNFreeVariableNameAndTypeList(raw, count)
    }
    unsafe fn wrap_raw<'a>(raw: &'a Self::Raw, _context: &'a Self::Context) -> Self::Wrapped<'a> {
        NamedTypedVariable::ref_from_raw(raw)
    }
}

////////////////////////
// EnumerationBuilder

#[derive(Debug, Clone)]
#[repr(C)]
pub struct EnumerationMember {
    name: BnString,
    value: u64,
    is_default: bool,
}

impl EnumerationMember {
    pub fn new<S: BnStrCompatible>(name: S, value: u64, is_default: bool) -> Self {
        Self {
            name: BnString::new(name),
            value,
            is_default: is_default,
        }
    }

    pub(crate) unsafe fn from_raw(raw: BNEnumerationMember) -> Self {
        Self {
            name: BnString::from_raw(raw.name),
            value: raw.value,
            is_default: raw.isDefault,
        }
    }

    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    pub fn value(&self) -> u64 {
        self.value
    }

    pub fn is_default(&self) -> bool {
        self.is_default
    }
}

#[derive(PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct EnumerationBuilder {
    pub(crate) handle: *mut BNEnumerationBuilder,
}

impl EnumerationBuilder {
    pub fn new() -> Self {
        Self {
            handle: unsafe { BNCreateEnumerationBuilder() },
        }
    }

    pub(crate) unsafe fn from_raw(handle: *mut BNEnumerationBuilder) -> Self {
        Self { handle }
    }

    pub fn finalize(&self) -> Enumeration {
        unsafe { Enumeration::from_raw(BNFinalizeEnumerationBuilder(self.handle)) }
    }

    pub fn append<S: BnStrCompatible>(&self, name: S) -> &Self {
        let name = name.into_bytes_with_nul();
        unsafe {
            BNAddEnumerationBuilderMember(self.handle, name.as_ref().as_ptr() as _);
        }
        self
    }

    pub fn insert<S: BnStrCompatible>(&self, name: S, value: u64) -> &Self {
        let name = name.into_bytes_with_nul();
        unsafe {
            BNAddEnumerationBuilderMemberWithValue(self.handle, name.as_ref().as_ptr() as _, value);
        }
        self
    }

    pub fn replace<S: BnStrCompatible>(&self, id: usize, name: S, value: u64) -> &Self {
        let name = name.into_bytes_with_nul();
        unsafe {
            BNReplaceEnumerationBuilderMember(self.handle, id, name.as_ref().as_ptr() as _, value);
        }
        self
    }

    pub fn remove(&self, id: usize) -> &Self {
        unsafe {
            BNRemoveEnumerationBuilderMember(self.handle, id);
        }

        self
    }

    pub fn members(&self) -> Vec<EnumerationMember> {
        unsafe {
            let mut count = 0;
            let members_raw = BNGetEnumerationBuilderMembers(self.handle, &mut count);
            let members: &[BNEnumerationMember] = slice::from_raw_parts(members_raw, count);

            let result = (0..count)
                .map(|i| EnumerationMember::from_raw(members[i]))
                .collect();

            BNFreeEnumerationMemberList(members_raw, count);

            result
        }
    }
}

impl Default for EnumerationBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl From<&Enumeration> for EnumerationBuilder {
    fn from(enumeration: &Enumeration) -> Self {
        unsafe {
            Self::from_raw(BNCreateEnumerationBuilderFromEnumeration(
                enumeration.handle,
            ))
        }
    }
}

impl Drop for EnumerationBuilder {
    fn drop(&mut self) {
        unsafe { BNFreeEnumerationBuilder(self.handle) };
    }
}

/////////////////
// Enumeration

#[derive(PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Enumeration {
    pub(crate) handle: *mut BNEnumeration,
}

impl Enumeration {
    pub(crate) unsafe fn from_raw(handle: *mut BNEnumeration) -> Self {
        debug_assert!(!handle.is_null());
        Self { handle }
    }

    pub fn builder() -> EnumerationBuilder {
        EnumerationBuilder::new()
    }

    pub fn members(&self) -> Vec<EnumerationMember> {
        unsafe {
            let mut count = 0;
            let members_raw = BNGetEnumerationMembers(self.handle, &mut count);
            let members: &[BNEnumerationMember] = slice::from_raw_parts(members_raw, count);

            let result = (0..count)
                .map(|i| EnumerationMember::from_raw(members[i]))
                .collect();

            BNFreeEnumerationMemberList(members_raw, count);

            result
        }
    }
}

impl Clone for Enumeration {
    fn clone(&self) -> Self {
        unsafe { Self::from_raw(BNNewEnumerationReference(self.handle)) }
    }
}

impl Drop for Enumeration {
    fn drop(&mut self) {
        unsafe {
            BNFreeEnumeration(self.handle);
        }
    }
}

//////////////////////
// StructureBuilder

pub type StructureType = BNStructureVariant;

#[derive(PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct StructureBuilder {
    pub(crate) handle: *mut BNStructureBuilder,
}

/// ```no_run
/// // Includes
/// # use binaryninja::binaryview::BinaryViewExt;
/// use binaryninja::types::{Structure, StructureBuilder, Type, MemberAccess, MemberScope};
///
/// // Define struct, set size (in bytes)
/// let mut my_custom_struct = StructureBuilder::new();
/// let field_1 = Type::named_int(5, false, "my_weird_int_type");
/// let field_2 = Type::int(4, false);
/// let field_3 = Type::int(8, false);
///
/// // Assign those fields
/// my_custom_struct.insert(&field_1, "field_1", 0, false, MemberAccess::PublicAccess, MemberScope::NoScope);
/// my_custom_struct.insert(&field_2, "field_2", 5, false, MemberAccess::PublicAccess, MemberScope::NoScope);
/// my_custom_struct.insert(&field_3, "field_3", 9, false, MemberAccess::PublicAccess, MemberScope::NoScope);
/// my_custom_struct.append(&field_1, "field_4", MemberAccess::PublicAccess, MemberScope::NoScope);
///
/// // Convert structure to type
/// let my_custom_structure_type = Type::structure(&my_custom_struct.finalize());
///
/// // Add the struct to the binary view to use in analysis
/// let bv = binaryninja::load("example").unwrap();
/// bv.define_user_type("my_custom_struct", &my_custom_structure_type);
/// ```
impl StructureBuilder {
    pub fn new() -> Self {
        Self {
            handle: unsafe { BNCreateStructureBuilder() },
        }
    }

    pub(crate) unsafe fn from_raw(handle: *mut BNStructureBuilder) -> Self {
        debug_assert!(!handle.is_null());
        Self { handle }
    }

    // Chainable terminal
    pub fn finalize(&self) -> Structure {
        unsafe { Structure::from_raw(BNFinalizeStructureBuilder(self.handle)) }
    }

    // Chainable builders/setters

    pub fn set_width(&self, width: u64) -> &Self {
        unsafe {
            BNSetStructureBuilderWidth(self.handle, width);
        }

        self
    }

    pub fn set_alignment(&self, alignment: usize) -> &Self {
        unsafe {
            BNSetStructureBuilderAlignment(self.handle, alignment);
        }

        self
    }

    pub fn set_packed(&self, packed: bool) -> &Self {
        unsafe {
            BNSetStructureBuilderPacked(self.handle, packed);
        }

        self
    }

    pub fn set_structure_type(&self, t: StructureType) -> &Self {
        unsafe { BNSetStructureBuilderType(self.handle, t) };
        self
    }

    pub fn set_pointer_offset(&self, offset: i64) -> &Self {
        unsafe { BNSetStructureBuilderPointerOffset(self.handle, offset) };
        self
    }

    pub fn set_propagates_data_var_refs(&self, does: bool) -> &Self {
        unsafe { BNSetStructureBuilderPropagatesDataVariableReferences(self.handle, does) };
        self
    }

    pub fn set_base_structures(&self, bases: Vec<BaseStructure>) -> &Self {
        let mut bases_api = vec![];
        for base in bases {
            bases_api.push(BNBaseStructure {
                type_: base.ty.handle,
                offset: base.offset,
                width: base.width,
            });
        }

        unsafe {
            BNSetBaseStructuresForStructureBuilder(
                self.handle,
                bases_api.as_mut_ptr(),
                bases_api.len(),
            )
        };

        self
    }

    pub fn append<'a, S: BnStrCompatible, T: Into<Conf<&'a Type>>>(
        &self,
        t: T,
        name: S,
        access: MemberAccess,
        scope: MemberScope,
    ) -> &Self {
        let name = name.into_bytes_with_nul();
        unsafe {
            BNAddStructureBuilderMember(
                self.handle,
                &t.into().into(),
                name.as_ref().as_ptr() as _,
                access,
                scope,
            );
        }

        self
    }

    pub fn insert_member(&self, member: &StructureMember, overwrite_existing: bool) -> &Self {
        self.insert(
            member.ty(),
            member.name(),
            member.offset(),
            overwrite_existing,
            member.access(),
            member.scope(),
        );
        self
    }

    pub fn insert<'a, S: BnStrCompatible, T: Into<Conf<&'a Type>>>(
        &self,
        t: T,
        name: S,
        offset: u64,
        overwrite_existing: bool,
        access: MemberAccess,
        scope: MemberScope,
    ) -> &Self {
        let name = name.into_bytes_with_nul();
        unsafe {
            BNAddStructureBuilderMemberAtOffset(
                self.handle,
                &t.into().into(),
                name.as_ref().as_ptr() as _,
                offset,
                overwrite_existing,
                access,
                scope,
            );
        }

        self
    }

    pub fn with_members<'a, S: BnStrCompatible, T: Into<Conf<&'a Type>>>(
        &self,
        members: impl IntoIterator<Item = (T, S)>,
    ) -> &Self {
        for (t, name) in members.into_iter() {
            self.append(t, name, MemberAccess::NoAccess, MemberScope::NoScope);
        }
        self
    }

    // Getters

    pub fn width(&self) -> u64 {
        unsafe { BNGetStructureBuilderWidth(self.handle) }
    }

    pub fn alignment(&self) -> usize {
        unsafe { BNGetStructureBuilderAlignment(self.handle) }
    }

    pub fn packed(&self) -> bool {
        unsafe { BNIsStructureBuilderPacked(self.handle) }
    }

    pub fn structure_type(&self) -> StructureType {
        unsafe { BNGetStructureBuilderType(self.handle) }
    }

    pub fn pointer_offset(&self) -> i64 {
        unsafe { BNGetStructureBuilderPointerOffset(self.handle) }
    }

    pub fn propagates_data_var_refs(&self) -> bool {
        unsafe { BNStructureBuilderPropagatesDataVariableReferences(self.handle) }
    }

    pub fn base_structures(&self) -> Result<Vec<BaseStructure>> {
        let mut count = 0usize;
        let bases = unsafe { BNGetBaseStructuresForStructureBuilder(self.handle, &mut count) };
        if bases.is_null() {
            Err(())
        } else {
            unsafe {
                let bases_slice = slice::from_raw_parts_mut(bases, count);

                let result = bases_slice
                    .iter()
                    .map(|base| BaseStructure::from_raw(base))
                    .collect::<Vec<_>>();

                BNFreeBaseStructureList(bases, count);

                Ok(result)
            }
        }
    }

    pub fn members(&self) -> Array<StructureMember> {
        let mut count = 0;
        let members_raw = unsafe { BNGetStructureBuilderMembers(self.handle, &mut count) };
        unsafe { Array::new(members_raw, count, ()) }
    }

    pub fn index_by_name(&self, name: &str) -> Option<usize> {
        self.members()
            .iter()
            .position(|member| member.name() == name)
    }

    pub fn index_by_offset(&self, offset: u64) -> Option<usize> {
        self.members()
            .iter()
            .position(|member| member.offset() == offset)
    }

    // Setters

    pub fn clear_members(&self) {
        let len = self.members().len();
        for idx in (0..len).rev() {
            self.remove(idx)
        }
    }

    pub fn add_members<'a>(&self, members: impl IntoIterator<Item = &'a StructureMember>) {
        for member in members {
            self.append(member.ty(), member.name(), member.access(), member.scope());
        }
    }

    pub fn set_members<'a>(&self, members: impl IntoIterator<Item = &'a StructureMember>) {
        self.clear_members();
        self.add_members(members);
    }

    pub fn remove(&self, index: usize) {
        unsafe { BNRemoveStructureBuilderMember(self.handle, index) }
    }

    pub fn replace(&self, index: usize, type_: Conf<&Type>, name: &str, overwrite: bool) {
        let name = name.into_bytes_with_nul();
        let name_ptr = name.as_ptr() as *const _;

        let raw_type_ = BNTypeWithConfidence {
            type_: type_.contents as *const Type as *mut _,
            confidence: type_.confidence,
        };
        unsafe {
            BNReplaceStructureBuilderMember(self.handle, index, &raw_type_, name_ptr, overwrite)
        }
    }
}

impl From<&Structure> for StructureBuilder {
    fn from(structure: &Structure) -> StructureBuilder {
        unsafe { Self::from_raw(BNCreateStructureBuilderFromStructure(structure.handle)) }
    }
}

impl From<Vec<StructureMember>> for StructureBuilder {
    fn from(members: Vec<StructureMember>) -> StructureBuilder {
        let builder = StructureBuilder::new();
        for m in members {
            builder.insert_member(&m, false);
        }
        builder
    }
}

impl Debug for StructureBuilder {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "StructureBuilder {{ ... }}")
    }
}

impl Drop for StructureBuilder {
    fn drop(&mut self) {
        unsafe { BNFreeStructureBuilder(self.handle) };
    }
}

impl Default for StructureBuilder {
    fn default() -> Self {
        Self::new()
    }
}

///////////////
// Structure

#[derive(PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Structure {
    pub(crate) handle: *mut BNStructure,
}

impl Structure {
    unsafe fn from_raw(handle: *mut BNStructure) -> Self {
        debug_assert!(!handle.is_null());
        Self { handle }
    }

    pub fn builder() -> StructureBuilder {
        StructureBuilder::new()
    }

    pub fn width(&self) -> u64 {
        unsafe { BNGetStructureWidth(self.handle) }
    }

    pub fn structure_type(&self) -> StructureType {
        unsafe { BNGetStructureType(self.handle) }
    }

    pub fn members(&self) -> Result<Vec<StructureMember>> {
        unsafe {
            let mut count = 0;
            let members_raw: *mut BNStructureMember =
                BNGetStructureMembers(self.handle, &mut count);
            if members_raw.is_null() {
                return Err(());
            }
            let members = slice::from_raw_parts(members_raw, count);

            let result = members
                .iter()
                .map(|base| StructureMember::from_raw(*base))
                .collect();

            BNFreeStructureMemberList(members_raw, count);

            Ok(result)
        }
    }

    pub fn base_structures(&self) -> Result<Vec<BaseStructure>> {
        let mut count = 0usize;
        let bases = unsafe { BNGetBaseStructuresForStructure(self.handle, &mut count) };
        if bases.is_null() {
            Err(())
        } else {
            let bases_slice = unsafe { slice::from_raw_parts_mut(bases, count) };

            let result = bases_slice
                .iter()
                .map(|base| unsafe { BaseStructure::from_raw(base) })
                .collect::<Vec<_>>();

            unsafe {
                BNFreeBaseStructureList(bases, count);
            }

            Ok(result)
        }
    }

    // TODO : The other methods in the python version (alignment, packed, type, members, remove, replace, etc)
}

impl Debug for Structure {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "Structure {{")?;
        if let Ok(members) = self.members() {
            for member in members {
                write!(f, " {:?}", member)?;
            }
        }
        write!(f, "}}")
    }
}

impl Clone for Structure {
    fn clone(&self) -> Self {
        unsafe { Self::from_raw(BNNewStructureReference(self.handle)) }
    }
}

impl Drop for Structure {
    fn drop(&mut self) {
        unsafe {
            BNFreeStructure(self.handle);
        }
    }
}

#[derive(Debug, Clone)]
#[repr(C)]
pub struct StructureMember {
    type_: Type,
    name: BnString,
    offset: u64,
    type_confidence: u8,
    access: MemberAccess,
    scope: MemberScope,
}

impl StructureMember {
    pub fn new<S: BnStrCompatible>(
        ty: Conf<Type>,
        name: S,
        offset: u64,
        access: MemberAccess,
        scope: MemberScope,
    ) -> Self {
        Self {
            type_: ty.contents,
            type_confidence: ty.confidence,
            name: BnString::new(name),
            offset,
            access,
            scope,
        }
    }

    pub(crate) unsafe fn from_raw(handle: BNStructureMember) -> Self {
        Self {
            type_: Type::from_raw(handle.type_),
            type_confidence: handle.typeConfidence,
            name: BnString::from_raw(handle.name),
            offset: handle.offset,
            access: handle.access,
            scope: handle.scope,
        }
    }

    pub fn ty(&self) -> Conf<&Type> {
        Conf::new(&self.type_, self.type_confidence)
    }

    pub fn set_type(&mut self, ty: Type) {
        self.type_ = ty;
    }

    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn access(&self) -> MemberAccess {
        self.access
    }

    pub fn scope(&self) -> MemberScope {
        self.scope
    }
}

impl CoreArrayProvider for StructureMember {
    type Raw = BNStructureMember;
    type Context = ();
    type Wrapped<'a> = Guard<'a, StructureMember>;
}

unsafe impl CoreArrayProviderInner for StructureMember {
    unsafe fn free(raw: *mut Self::Raw, count: usize, _context: &Self::Context) {
        BNFreeStructureMemberList(raw, count)
    }
    unsafe fn wrap_raw<'a>(raw: &'a Self::Raw, _context: &'a Self::Context) -> Self::Wrapped<'a> {
        Guard::new(StructureMember::from_raw(*raw), &())
    }
}

#[derive(Debug, Clone)]
pub struct InheritedStructureMember {
    pub base: NamedTypeReference,
    pub base_offset: u64,
    pub member: StructureMember,
    pub member_index: usize,
}

impl InheritedStructureMember {
    pub fn new(
        base: NamedTypeReference,
        base_offset: u64,
        member: StructureMember,
        member_index: usize,
    ) -> Self {
        Self {
            base,
            base_offset,
            member,
            member_index,
        }
    }

    // pub(crate) unsafe fn from_raw(handle: BNInheritedStructureMember) -> Self {
    //     Self {
    //         base: RefCountable::inc_ref(&NamedTypeReference::from_raw(handle.base)),
    //         base_offset: handle.baseOffset,
    //         member: StructureMember::from_raw(handle.member),
    //         member_index: handle.memberIndex,
    //     }
    // }
}

#[derive(Debug, Clone)]
pub struct BaseStructure {
    pub ty: NamedTypeReference,
    pub offset: u64,
    pub width: u64,
}

impl BaseStructure {
    pub fn new(ty: NamedTypeReference, offset: u64, width: u64) -> Self {
        Self { ty, offset, width }
    }

    pub(crate) unsafe fn from_raw(handle: &BNBaseStructure) -> Self {
        // we don't own the `NamedTypeReference` handle, we should create a new
        // Type, but not drop the received one
        let new_type = core::mem::ManuallyDrop::new(NamedTypeReference::from_raw(handle.type_));
        Self {
            ty: (*new_type).clone(),
            offset: handle.offset,
            width: handle.width,
        }
    }
}

////////////////////////
// NamedTypeReference

#[derive(PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct NamedTypeReference {
    pub(crate) handle: *mut BNNamedTypeReference,
}

impl NamedTypeReference {
    pub(crate) unsafe fn from_raw(handle: *mut BNNamedTypeReference) -> Self {
        debug_assert!(!handle.is_null());

        Self { handle }
    }

    /// Create an NTR to a type that did not come directly from a BinaryView's types list.
    /// That is to say, if you're referencing a new type you're GOING to add, use this.
    /// You should not assign type ids yourself, that is the responsibility of the BinaryView
    /// implementation after your types have been added. Just make sure the names match up and
    /// the core will do the id stuff for you.
    pub fn new(type_class: NamedTypeReferenceClass, mut name: QualifiedName) -> Self {
        // TODO BNCreateNamedType create a type with counter 0? Is it possible?
        unsafe {
            // BNNewNamedTypeReference
            Self::from_raw(BNCreateNamedType(
                type_class,
                ptr::null() as *const _,
                &mut name.0,
            ))
        }
    }

    /// Create an NTR to a type with an existing type id, which generally means it came directly
    /// from a BinaryView's types list and its id was looked up using `BinaryView::get_type_id`.
    /// You should not assign type ids yourself: if you use this to reference a type you are going
    /// to create but have not yet created, you may run into problems when giving your types to
    /// a BinaryView.
    pub fn new_with_id<S: BnStrCompatible>(
        type_class: NamedTypeReferenceClass,
        type_id: S,
        mut name: QualifiedName,
    ) -> Self {
        let type_id = type_id.into_bytes_with_nul();

        // TODO BNCreateNamedType create a type with counter 0? Is it possible?
        unsafe {
            // BNNewNamedTypeReference
            Self::from_raw(BNCreateNamedType(
                type_class,
                type_id.as_ref().as_ptr() as _,
                &mut name.0,
            ))
        }
    }

    pub fn name(&self) -> QualifiedName {
        let named_ref: BNQualifiedName = unsafe { BNGetTypeReferenceName(self.handle) };
        QualifiedName(named_ref)
    }

    pub fn id(&self) -> BnString {
        unsafe { BnString::from_raw(BNGetTypeReferenceId(self.handle)) }
    }

    pub fn class(&self) -> NamedTypeReferenceClass {
        unsafe { BNGetTypeReferenceClass(self.handle) }
    }

    fn target_helper(&self, bv: &BinaryView, visited: &mut HashSet<BnString>) -> Option<Type> {
        // TODO : This is a clippy bug (#10088, I think); remove after we upgrade past 2022-12-12
        #[allow(clippy::manual_filter)]
        if let Some(t) = bv.get_type_by_id(self.id()) {
            if t.type_class() != TypeClass::NamedTypeReferenceClass {
                Some(t)
            } else {
                let t = t.get_named_type_reference().unwrap();
                if visited.contains(&t.id()) {
                    error!("Can't get target for recursively defined type!");
                    None
                } else {
                    visited.insert(t.id());
                    t.target_helper(bv, visited)
                }
            }
        } else {
            None
        }
    }

    pub fn target(&self, bv: &BinaryView) -> Option<Type> {
        //! Returns the type referenced by this named type reference
        self.target_helper(bv, &mut HashSet::new())
    }
}

impl Clone for NamedTypeReference {
    fn clone(&self) -> Self {
        unsafe { Self::from_raw(BNNewNamedTypeReference(self.handle)) }
    }
}

impl Drop for NamedTypeReference {
    fn drop(&mut self) {
        unsafe { BNFreeNamedTypeReference(self.handle) }
    }
}

impl Debug for NamedTypeReference {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{} (id: {})", self.name(), self.id())
    }
}

///////////////////
// QualifiedName

#[repr(transparent)]
pub struct QualifiedName(pub(crate) BNQualifiedName);

impl QualifiedName {
    // TODO : I think this is bad
    pub fn string(&self) -> String {
        unsafe {
            slice::from_raw_parts(self.0.name, self.0.nameCount)
                .iter()
                .map(|c| CStr::from_ptr(*c).to_string_lossy())
                .collect::<Vec<_>>()
                .join("::")
        }
    }

    pub fn join(&self) -> Cow<str> {
        let join: *mut c_char = self.0.join;
        unsafe { CStr::from_ptr(join) }.to_string_lossy()
    }

    pub fn strings(&self) -> Vec<Cow<str>> {
        let names: *mut *mut c_char = self.0.name;
        unsafe {
            slice::from_raw_parts(names, self.0.nameCount)
                .iter()
                .map(|name| CStr::from_ptr(*name).to_string_lossy())
                .collect::<Vec<_>>()
        }
    }

    pub fn len(&self) -> usize {
        self.0.nameCount
    }

    pub fn is_empty(&self) -> bool {
        self.0.nameCount == 0
    }
}

impl<S: BnStrCompatible> From<S> for QualifiedName {
    fn from(name: S) -> Self {
        let join = BnString::new("::");
        let name = name.into_bytes_with_nul();
        let mut list = vec![name.as_ref().as_ptr() as *const _];

        QualifiedName(BNQualifiedName {
            name: unsafe { BNAllocStringList(list.as_mut_ptr(), 1) },
            join: join.into_raw(),
            nameCount: 1,
        })
    }
}

impl<S: BnStrCompatible> From<Vec<S>> for QualifiedName {
    fn from(names: Vec<S>) -> Self {
        let join = BnString::new("::");
        let names = names
            .into_iter()
            .map(|n| n.into_bytes_with_nul())
            .collect::<Vec<_>>();
        let mut list = names
            .iter()
            .map(|n| n.as_ref().as_ptr() as *const _)
            .collect::<Vec<_>>();

        QualifiedName(BNQualifiedName {
            name: unsafe { BNAllocStringList(list.as_mut_ptr(), list.len()) },
            join: join.into_raw(),
            nameCount: list.len(),
        })
    }
}

impl Clone for QualifiedName {
    fn clone(&self) -> Self {
        let strings = self.strings();
        let name = Self::from(strings.iter().collect::<Vec<&Cow<str>>>());
        name
    }
}

impl Hash for QualifiedName {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.join().hash(state);
        self.strings().hash(state);
    }
}

impl Debug for QualifiedName {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.string())
    }
}

impl Display for QualifiedName {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.string())
    }
}

impl PartialEq for QualifiedName {
    fn eq(&self, other: &Self) -> bool {
        self.strings() == other.strings()
    }
}

impl Eq for QualifiedName {}

impl Drop for QualifiedName {
    fn drop(&mut self) {
        unsafe {
            BNFreeQualifiedName(&mut self.0);
        }
    }
}

impl CoreArrayProvider for QualifiedName {
    type Raw = BNQualifiedName;
    type Context = ();
    type Wrapped<'a> = &'a QualifiedName;
}
unsafe impl CoreArrayProviderInner for QualifiedName {
    unsafe fn free(raw: *mut Self::Raw, count: usize, _context: &Self::Context) {
        BNFreeTypeNameList(raw, count);
    }
    unsafe fn wrap_raw<'a>(raw: &'a Self::Raw, _context: &'a Self::Context) -> Self::Wrapped<'a> {
        mem::transmute(raw)
    }
}

//////////////////////////
// QualifiedNameAndType

#[repr(transparent)]
pub struct QualifiedNameAndType(pub(crate) BNQualifiedNameAndType);

impl QualifiedNameAndType {
    pub fn name(&self) -> &QualifiedName {
        unsafe { mem::transmute(&self.0.name) }
    }

    pub fn type_object(&self) -> &Type {
        unsafe { &*(self.0.type_ as *mut Type) }
    }
}

impl Drop for QualifiedNameAndType {
    fn drop(&mut self) {
        unsafe {
            BNFreeQualifiedNameAndType(&mut self.0);
        }
    }
}

impl CoreArrayProvider for QualifiedNameAndType {
    type Raw = BNQualifiedNameAndType;
    type Context = ();
    type Wrapped<'a> = &'a QualifiedNameAndType;
}
unsafe impl CoreArrayProviderInner for QualifiedNameAndType {
    unsafe fn free(raw: *mut Self::Raw, count: usize, _context: &Self::Context) {
        BNFreeTypeAndNameList(raw, count);
    }
    unsafe fn wrap_raw<'a>(raw: &'a Self::Raw, _context: &'a Self::Context) -> Self::Wrapped<'a> {
        mem::transmute(raw)
    }
}

//////////////////////////
// QualifiedNameTypeAndId

#[repr(transparent)]
pub struct QualifiedNameTypeAndId(pub(crate) BNQualifiedNameTypeAndId);

impl QualifiedNameTypeAndId {
    pub fn name(&self) -> &QualifiedName {
        unsafe { mem::transmute(&self.0.name) }
    }

    pub fn id(&self) -> &str {
        unsafe { CStr::from_ptr(self.0.id).to_str().unwrap() }
    }

    pub fn type_object(&self) -> &Type {
        unsafe { &*(self.0.type_ as *mut Type) }
    }
}

impl Drop for QualifiedNameTypeAndId {
    fn drop(&mut self) {
        unsafe {
            BNFreeQualifiedNameTypeAndId(&mut self.0);
        }
    }
}

impl CoreArrayProvider for QualifiedNameTypeAndId {
    type Raw = BNQualifiedNameTypeAndId;
    type Context = ();
    type Wrapped<'a> = &'a QualifiedNameTypeAndId;
}
unsafe impl CoreArrayProviderInner for QualifiedNameTypeAndId {
    unsafe fn free(raw: *mut Self::Raw, count: usize, _context: &Self::Context) {
        BNFreeTypeIdList(raw, count);
    }
    unsafe fn wrap_raw<'a>(raw: &'a Self::Raw, _context: &'a Self::Context) -> Self::Wrapped<'a> {
        mem::transmute(raw)
    }
}

//////////////////////////
// NameAndType

pub struct NameAndType {
    pub name: String,
    pub t: Conf<Type>,
}

impl NameAndType {
    pub(crate) fn from_raw(raw: &BNNameAndType) -> Self {
        Self::new(
            raw_to_string(raw.name).unwrap(),
            unsafe { Type::from_raw(raw.type_) },
            raw.typeConfidence,
        )
    }
    pub(crate) fn into_raw(self) -> BNNameAndType {
        BNNameAndType {
            name: BnString::new(self.name).into_raw(),
            type_: self.t.contents.handle,
            typeConfidence: self.t.confidence,
        }
    }
    pub fn new(name: String, t: Type, confidence: u8) -> Self {
        Self {
            name,
            t: Conf::new(t, confidence),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn type_with_confidence(&self) -> Conf<Type> {
        self.t.clone()
    }
}

impl Clone for NameAndType {
    fn clone(&self) -> Self {
        let type_ = self.type_with_confidence();
        Self::new(self.name.clone(), type_.contents, type_.confidence)
    }
}

impl CoreArrayProvider for NameAndType {
    type Raw = BNNameAndType;
    type Context = ();
    type Wrapped<'a> = Guard<'a, NameAndType>;
}

unsafe impl CoreArrayProviderInner for NameAndType {
    unsafe fn free(raw: *mut Self::Raw, count: usize, _context: &Self::Context) {
        BNFreeNameAndTypeList(raw, count);
    }
    unsafe fn wrap_raw<'a>(raw: &'a Self::Raw, _context: &'a Self::Context) -> Self::Wrapped<'a> {
        unsafe { Guard::new(NameAndType::from_raw(raw), raw) }
    }
}

//////////////////
// DataVariable

#[repr(transparent)]
pub struct DataVariable(pub(crate) BNDataVariable);

// impl DataVariable {
//     pub(crate) fn from_raw(var: &BNDataVariable) -> Self {
//         let var = DataVariable(*var);
//         Self(BNDataVariable {
//             type_: unsafe { Ref::into_raw(var.t().to_owned()).handle },
//             ..var.0
//         })
//     }
// }

impl DataVariable {
    pub fn address(&self) -> u64 {
        self.0.address
    }

    pub fn auto_discovered(&self) -> bool {
        self.0.autoDiscovered
    }

    pub fn t(&self) -> &Type {
        unsafe { mem::transmute(&self.0.type_) }
    }

    pub fn type_with_confidence(&self) -> Conf<&Type> {
        Conf::new(self.t(), self.0.typeConfidence)
    }

    pub fn symbol(&self, bv: &BinaryView) -> Option<Symbol> {
        bv.symbol_by_address(self.0.address).ok()
    }
}

impl Clone for DataVariable {
    fn clone(&self) -> Self {
        Self(BNDataVariable {
            type_: self.t().clone().handle,
            ..self.0
        })
    }
}

impl Drop for DataVariable {
    fn drop(&mut self) {
        unsafe { BNFreeType(self.0.type_) }
    }
}

impl CoreArrayProvider for DataVariable {
    type Raw = BNDataVariable;
    type Context = ();
    type Wrapped<'a> = &'a DataVariable;
}
unsafe impl CoreArrayProviderInner for DataVariable {
    unsafe fn free(raw: *mut Self::Raw, count: usize, _context: &Self::Context) {
        BNFreeDataVariables(raw, count);
    }
    unsafe fn wrap_raw<'a>(raw: &'a Self::Raw, _context: &'a Self::Context) -> Self::Wrapped<'a> {
        mem::transmute(raw)
    }
}

/////////////////////////
// DataVariableAndName

pub struct DataVariableAndName<S: BnStrCompatible> {
    pub address: u64,
    pub t: Conf<Type>,
    pub auto_discovered: bool,
    pub name: S,
}

impl DataVariableAndName<String> {
    pub(crate) fn from_raw(var: &BNDataVariableAndName) -> Self {
        Self {
            address: var.address,
            t: Conf::new(unsafe { Type::from_raw(var.type_) }, var.typeConfidence),
            auto_discovered: var.autoDiscovered,
            name: raw_to_string(var.name).unwrap(),
        }
    }
}

impl<S: BnStrCompatible> DataVariableAndName<S> {
    pub fn new(address: u64, t: Conf<Type>, auto_discovered: bool, name: S) -> Self {
        Self {
            address,
            t,
            auto_discovered,
            name,
        }
    }

    pub fn type_with_confidence(&self) -> Conf<Type> {
        Conf::new(self.t.contents.clone(), self.t.confidence)
    }
}

/////////////////////////
// ILIntrinsic

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ILIntrinsic {
    arch: CoreArchitecture,
    index: u32,
}

impl ILIntrinsic {
    pub(crate) fn new(arch: CoreArchitecture, index: u32) -> Self {
        Self { arch, index }
    }

    pub fn name(&self) -> &str {
        let name_ptr = unsafe { BNGetArchitectureIntrinsicName(self.arch.0, self.index) };
        let name_raw = unsafe { core::ffi::CStr::from_ptr(name_ptr) };
        name_raw.to_str().unwrap()
    }

    // TODO impl inputs and outputs function
}

/////////////////////////
// RegisterValueType

#[derive(Debug, Copy, Clone, Hash, PartialEq, Eq)]
pub enum RegisterValueType {
    UndeterminedValue,
    EntryValue,
    ConstantValue,
    ConstantPointerValue,
    ExternalPointerValue,
    StackFrameOffset,
    ReturnAddressValue,
    ImportedAddressValue,
    SignedRangeValue,
    UnsignedRangeValue,
    LookupTableValue,
    InSetOfValues,
    NotInSetOfValues,
    ConstantDataValue,
    ConstantDataZeroExtendValue,
    ConstantDataSignExtendValue,
    ConstantDataAggregateValue,
}

impl RegisterValueType {
    pub(crate) fn from_raw_value(value: u32) -> Option<Self> {
        use BNRegisterValueType::*;
        Some(match value {
            x if x == UndeterminedValue as u32 => Self::UndeterminedValue,
            x if x == EntryValue as u32 => Self::EntryValue,
            x if x == ConstantValue as u32 => Self::ConstantValue,
            x if x == ConstantPointerValue as u32 => Self::ConstantPointerValue,
            x if x == ExternalPointerValue as u32 => Self::ExternalPointerValue,
            x if x == StackFrameOffset as u32 => Self::StackFrameOffset,
            x if x == ReturnAddressValue as u32 => Self::ReturnAddressValue,
            x if x == ImportedAddressValue as u32 => Self::ImportedAddressValue,
            x if x == SignedRangeValue as u32 => Self::SignedRangeValue,
            x if x == UnsignedRangeValue as u32 => Self::UnsignedRangeValue,
            x if x == LookupTableValue as u32 => Self::LookupTableValue,
            x if x == InSetOfValues as u32 => Self::InSetOfValues,
            x if x == NotInSetOfValues as u32 => Self::NotInSetOfValues,
            x if x == ConstantDataValue as u32 => Self::ConstantDataValue,
            x if x == ConstantDataZeroExtendValue as u32 => Self::ConstantDataZeroExtendValue,
            x if x == ConstantDataSignExtendValue as u32 => Self::ConstantDataSignExtendValue,
            x if x == ConstantDataAggregateValue as u32 => Self::ConstantDataAggregateValue,
            _ => return None,
        })
    }

    pub(crate) fn into_raw_value(self) -> BNRegisterValueType {
        use BNRegisterValueType::*;
        match self {
            Self::UndeterminedValue => UndeterminedValue,
            Self::EntryValue => EntryValue,
            Self::ConstantValue => ConstantValue,
            Self::ConstantPointerValue => ConstantPointerValue,
            Self::ExternalPointerValue => ExternalPointerValue,
            Self::StackFrameOffset => StackFrameOffset,
            Self::ReturnAddressValue => ReturnAddressValue,
            Self::ImportedAddressValue => ImportedAddressValue,
            Self::SignedRangeValue => SignedRangeValue,
            Self::UnsignedRangeValue => UnsignedRangeValue,
            Self::LookupTableValue => LookupTableValue,
            Self::InSetOfValues => InSetOfValues,
            Self::NotInSetOfValues => NotInSetOfValues,
            Self::ConstantDataValue => ConstantDataValue,
            Self::ConstantDataZeroExtendValue => ConstantDataZeroExtendValue,
            Self::ConstantDataSignExtendValue => ConstantDataSignExtendValue,
            Self::ConstantDataAggregateValue => ConstantDataAggregateValue,
        }
    }
}

/////////////////////////
// RegisterValue

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct RegisterValue {
    pub(crate) state: RegisterValueType,
    pub(crate) value: i64,
    pub(crate) offset: i64,
    pub(crate) size: usize,
}

impl RegisterValue {
    pub fn new(state: RegisterValueType, value: i64, offset: i64, size: usize) -> Self {
        Self {
            state,
            value,
            offset,
            size,
        }
    }
}

impl From<BNRegisterValue> for RegisterValue {
    fn from(value: BNRegisterValue) -> Self {
        Self {
            state: RegisterValueType::from_raw_value(value.state as u32).unwrap(),
            value: value.value,
            offset: value.offset,
            size: value.size,
        }
    }
}

impl From<RegisterValue> for BNRegisterValue {
    fn from(value: RegisterValue) -> Self {
        Self {
            state: value.state.into_raw_value(),
            value: value.value,
            offset: value.offset,
            size: value.size,
        }
    }
}

/////////////////////////
// ConstantData

#[derive(Clone, Debug, PartialEq, Hash)]
pub struct ConstantData {
    function: Function,
    value: RegisterValue,
}

impl ConstantData {
    pub(crate) fn new(function: Function, value: RegisterValue) -> Self {
        Self { function, value }
    }
}

// unsafe impl<S: BnStrCompatible> CoreArrayProvider for DataVariableAndName<S> {
//     type Raw = BNDataVariableAndName;
//     type Context = ();
// }

// unsafe impl<S: BnStrCompatible> CoreOwnedArrayProvider for DataVariableAndName<S> {
//     unsafe fn free(raw: *mut Self::Raw, count: usize, _context: &Self::Context) {
//         BNFreeDataVariablesAndName(raw, count);
//     }
// }

// unsafe impl<'a, S: 'a + BnStrCompatible> CoreArrayWrapper<'a> for DataVariableAndName<S> {
//     type Wrapped = &'a DataVariableAndName<S>;
// }

// unsafe impl<'a, S: 'a + BnStrCompatible> CoreArrayWrapper<'a> for DataVariableAndName<S> {
//     unsafe fn wrap_raw(raw: &'a Self::Raw, _context: &'a Self::Context) -> Self::Wrapped {
//         mem::transmute(raw)
//     }
// }
