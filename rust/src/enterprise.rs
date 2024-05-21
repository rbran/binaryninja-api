use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::string::{BnStrCompatible, BnString};

pub fn server_username() -> BnString {
    unsafe { BnString::from_raw(binaryninjacore_sys::BNGetEnterpriseServerUsername()) }
}

pub fn server_url() -> BnString {
    unsafe { BnString::from_raw(binaryninjacore_sys::BNGetEnterpriseServerUrl()) }
}

pub fn set_server_url<S: BnStrCompatible>(url: S) -> Result<(), ()> {
    let url = url.into_bytes_with_nul();
    let result = unsafe {
        binaryninjacore_sys::BNSetEnterpriseServerUrl(url.as_ref().as_ptr() as *const i8)
    };
    if result {
        Ok(())
    } else {
        Err(())
    }
}

pub fn server_name() -> BnString {
    unsafe { BnString::from_raw(binaryninjacore_sys::BNGetEnterpriseServerName()) }
}

pub fn server_id() -> BnString {
    unsafe { BnString::from_raw(binaryninjacore_sys::BNGetEnterpriseServerId()) }
}

pub fn server_version() -> u64 {
    unsafe { binaryninjacore_sys::BNGetEnterpriseServerVersion() }
}

pub fn server_build_id() -> BnString {
    unsafe { BnString::from_raw(binaryninjacore_sys::BNGetEnterpriseServerBuildId()) }
}

pub fn server_token() -> BnString {
    unsafe { BnString::from_raw(binaryninjacore_sys::BNGetEnterpriseServerToken()) }
}

pub fn license_duration() -> Duration {
    Duration::from_secs(unsafe { binaryninjacore_sys::BNGetEnterpriseServerLicenseDuration() })
}

pub fn license_expiration_time() -> SystemTime {
    let m = Duration::from_secs(unsafe {
        binaryninjacore_sys::BNGetEnterpriseServerLicenseExpirationTime()
    });
    UNIX_EPOCH + m
}

pub fn server_reservation_time_limit() -> Duration {
    Duration::from_secs(unsafe { binaryninjacore_sys::BNGetEnterpriseServerReservationTimeLimit() })
}

pub fn is_server_floating_license() -> bool {
    unsafe { binaryninjacore_sys::BNIsEnterpriseServerFloatingLicense() }
}

pub fn is_server_license_still_activated() -> bool {
    unsafe { binaryninjacore_sys::BNIsEnterpriseServerLicenseStillActivated() }
}

pub fn authenticate_server_with_credentials<U, P>(username: U, password: P, remember: bool) -> bool
where
    U: BnStrCompatible,
    P: BnStrCompatible,
{
    let username = username.into_bytes_with_nul();
    let password = password.into_bytes_with_nul();
    unsafe {
        binaryninjacore_sys::BNAuthenticateEnterpriseServerWithCredentials(
            username.as_ref().as_ptr() as *const i8,
            password.as_ref().as_ptr() as *const i8,
            remember,
        )
    }
}

pub fn authenticate_server_with_method<S: BnStrCompatible>(method: S, remember: bool) -> bool {
    let method = method.into_bytes_with_nul();
    unsafe {
        binaryninjacore_sys::BNAuthenticateEnterpriseServerWithMethod(
            method.as_ref().as_ptr() as *const i8,
            remember,
        )
    }
}

pub fn connect_server() -> bool {
    unsafe { binaryninjacore_sys::BNConnectEnterpriseServer() }
}

pub fn deauthenticate_server() -> bool {
    unsafe { binaryninjacore_sys::BNDeauthenticateEnterpriseServer() }
}

pub fn cancel_server_authentication() {
    unsafe { binaryninjacore_sys::BNCancelEnterpriseServerAuthentication() }
}

pub fn update_server_license(timeout: Duration) -> bool {
    unsafe { binaryninjacore_sys::BNUpdateEnterpriseServerLicense(timeout.as_secs()) }
}

pub fn release_server_license() -> bool {
    unsafe { binaryninjacore_sys::BNReleaseEnterpriseServerLicense() }
}

pub fn is_server_connected() -> bool {
    unsafe { binaryninjacore_sys::BNIsEnterpriseServerConnected() }
}

pub fn is_server_authenticated() -> bool {
    unsafe { binaryninjacore_sys::BNIsEnterpriseServerAuthenticated() }
}

pub fn is_server_initialized() -> bool {
    unsafe { binaryninjacore_sys::BNIsEnterpriseServerInitialized() }
}

pub fn server_last_error() -> BnString {
    unsafe { BnString::from_raw(binaryninjacore_sys::BNGetEnterpriseServerLastError()) }
}
