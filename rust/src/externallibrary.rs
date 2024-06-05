use core::ffi;
use std::{mem, ptr};

use binaryninjacore_sys::*;

use crate::{
    project::ProjectFile,
    rc::{CoreArrayProvider, CoreArrayProviderInner, Ref},
    string::{BnStrCompatible, BnString},
    symbol::Symbol,
};

#[repr(transparent)]
pub struct ExternalLibrary {
    handle: ptr::NonNull<BNExternalLibrary>,
}

impl ExternalLibrary {
    pub(crate) unsafe fn from_raw(handle: ptr::NonNull<BNExternalLibrary>) -> Self {
        Self { handle }
    }

    pub(crate) unsafe fn ref_from_raw(handle: &*mut BNExternalLibrary) -> &Self {
        debug_assert!(!handle.is_null());
        mem::transmute(handle)
    }

    #[allow(clippy::mut_from_ref)]
    pub(crate) unsafe fn as_raw(&self) -> &mut BNExternalLibrary {
        &mut *self.handle.as_ptr()
    }

    pub fn name(&self) -> BnString {
        unsafe { BnString::from_raw(BNExternalLibraryGetName(self.as_raw())) }
    }

    pub fn backing_file(&self) -> ProjectFile {
        unsafe {
            ProjectFile::from_raw(
                ptr::NonNull::new(BNExternalLibraryGetBackingFile(self.as_raw())).unwrap(),
            )
        }
    }

    pub fn set_backing_file(&self, value: &ProjectFile) {
        unsafe { BNExternalLibrarySetBackingFile(self.as_raw(), value.as_raw()) }
    }
}

impl Clone for ExternalLibrary {
    fn clone(&self) -> Self {
        unsafe {
            Self::from_raw(ptr::NonNull::new(BNNewExternalLibraryReference(self.as_raw())).unwrap())
        }
    }
}

impl Drop for ExternalLibrary {
    fn drop(&mut self) {
        unsafe { BNFreeExternalLibrary(self.as_raw()) }
    }
}

impl CoreArrayProvider for ExternalLibrary {
    type Raw = *mut BNExternalLibrary;
    type Context = ();
    type Wrapped<'a> = &'a Self;
}

unsafe impl CoreArrayProviderInner for ExternalLibrary {
    unsafe fn free(raw: *mut Self::Raw, count: usize, _context: &Self::Context) {
        BNFreeExternalLibraryList(raw, count)
    }

    unsafe fn wrap_raw<'a>(raw: &'a Self::Raw, _context: &'a Self::Context) -> Self::Wrapped<'a> {
        Self::ref_from_raw(raw)
    }
}

pub struct ExternalLocation {
    handle: ptr::NonNull<BNExternalLocation>,
}

impl ExternalLocation {
    pub(crate) unsafe fn from_raw(handle: ptr::NonNull<BNExternalLocation>) -> Self {
        Self { handle }
    }

    pub(crate) unsafe fn ref_from_raw(handle: &*mut BNExternalLocation) -> &Self {
        debug_assert!(!handle.is_null());
        mem::transmute(handle)
    }

    #[allow(clippy::mut_from_ref)]
    pub(crate) unsafe fn as_raw(&self) -> &mut BNExternalLocation {
        &mut *self.handle.as_ptr()
    }

    /// Get the source symbol for this ExternalLocation
    pub fn source_symbol(&self) -> Ref<Symbol> {
        unsafe {
            let result = BNExternalLocationGetSourceSymbol(self.as_raw());
            Symbol::ref_from_raw(result)
        }
    }

    /// Check if this ExternalLocation has a target address
    pub fn has_target_address(&self) -> bool {
        unsafe { BNExternalLocationHasTargetAddress(self.as_raw()) }
    }

    /// Get the address pointed to by this ExternalLocation
    pub fn target_address(&self) -> Option<u64> {
        self.has_target_address()
            .then(|| unsafe { BNExternalLocationGetTargetAddress(self.as_raw()) })
    }

    /// Set the address pointed to by this ExternalLocation.
    /// ExternalLocations must have a valid target address and/or symbol set.
    ///
    /// * `new_address' - The address that this ExternalLocation will point to
    /// return `true` if the address was set, `false` otherwise
    pub fn set_target_address(&self, address: Option<u64>) -> bool {
        let addr_ptr = address
            .as_ref()
            .map(|x| x as *const u64 as *mut u64)
            .unwrap_or(ptr::null_mut());
        unsafe { BNExternalLocationSetTargetAddress(self.as_raw(), addr_ptr) }
    }

    /// Check if this ExternalLocation has a target symbol
    pub fn has_target_symbol(&self) -> bool {
        unsafe { BNExternalLocationHasTargetSymbol(self.as_raw()) }
    }

    /// Get the symbol pointed to by this ExternalLocation
    pub fn target_symbol(&self) -> Option<BnString> {
        self.has_target_symbol().then(|| unsafe {
            let result = BNExternalLocationGetTargetSymbol(self.as_raw());
            BnString::from_raw(result)
        })
    }

    /// Set the symbol pointed to by this ExternalLocation.
    /// ExternalLocations must have a valid target address and/or symbol set.
    pub fn set_target_symbol<S: BnStrCompatible>(&self, symbol: Option<S>) -> bool {
        let symbol_raw = symbol.map(|x| x.into_bytes_with_nul());
        let symbol_ptr = symbol_raw
            .as_ref()
            .map(|x| <S::Result as AsRef<[u8]>>::as_ref(x).as_ptr() as *const ffi::c_char)
            .unwrap_or(ptr::null_mut());
        unsafe { BNExternalLocationSetTargetSymbol(self.as_raw(), symbol_ptr) }
    }

    /// Get the ExternalLibrary that this ExternalLocation targets
    pub fn library(&self) -> Option<ExternalLibrary> {
        unsafe {
            let handle = BNExternalLocationGetExternalLibrary(self.as_raw());
            ptr::NonNull::new(handle).map(|handle| ExternalLibrary::from_raw(handle))
        }
    }

    /// Set the ExternalLibrary that this ExternalLocation targets
    pub fn set_library(&self, value: Option<ExternalLibrary>) {
        unsafe {
            let value_ptr = value
                .as_ref()
                .map(|lib| lib.as_raw() as *mut _)
                .unwrap_or(ptr::null_mut());
            BNExternalLocationSetExternalLibrary(self.as_raw(), value_ptr)
        }
    }
}

impl Clone for ExternalLocation {
    fn clone(&self) -> Self {
        unsafe {
            Self::from_raw(
                ptr::NonNull::new(BNNewExternalLocationReference(self.as_raw())).unwrap(),
            )
        }
    }
}

impl Drop for ExternalLocation {
    fn drop(&mut self) {
        unsafe { BNFreeExternalLocation(self.as_raw()) }
    }
}

impl CoreArrayProvider for ExternalLocation {
    type Raw = *mut BNExternalLocation;
    type Context = ();
    type Wrapped<'a> = &'a Self;
}

unsafe impl CoreArrayProviderInner for ExternalLocation {
    unsafe fn free(raw: *mut Self::Raw, count: usize, _context: &Self::Context) {
        BNFreeExternalLocationList(raw, count)
    }

    unsafe fn wrap_raw<'a>(raw: &'a Self::Raw, _context: &'a Self::Context) -> Self::Wrapped<'a> {
        Self::ref_from_raw(raw)
    }
}
