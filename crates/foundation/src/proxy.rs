//! CFNetwork's proxy questions, answered as a Mac with no proxy configured.
//!
//! A program asks before it opens a connection - Unity before every web
//! request - and an unbound answer is a call through null. The host's own
//! proxy settings are deliberately not read: the connection is made by host
//! libraries that read them themselves, so passing them on here would have the
//! guest route through a proxy twice.

#![allow(non_snake_case)]

use corefoundation::base::CFTypeRef;
use corefoundation::callbacks::Policy;

/// The proxy dictionary keys and proxy types. CFNetwork's values are their
/// own names: a proxy entry reads `kCFProxyTypeKey = kCFProxyTypeHTTP`.
pub const KEYS: &[(&str, &str)] = &[
    ("_kCFProxyTypeKey", "kCFProxyTypeKey"),
    ("_kCFProxyTypeNone", "kCFProxyTypeNone"),
    ("_kCFProxyTypeHTTP", "kCFProxyTypeHTTP"),
    ("_kCFProxyTypeHTTPS", "kCFProxyTypeHTTPS"),
    ("_kCFProxyTypeSOCKS", "kCFProxyTypeSOCKS"),
    ("_kCFProxyTypeFTP", "kCFProxyTypeFTP"),
    (
        "_kCFProxyTypeAutoConfigurationURL",
        "kCFProxyTypeAutoConfigurationURL",
    ),
    (
        "_kCFProxyTypeAutoConfigurationJavaScript",
        "kCFProxyTypeAutoConfigurationJavaScript",
    ),
    (
        "_kCFProxyAutoConfigurationURLKey",
        "kCFProxyAutoConfigurationURLKey",
    ),
    (
        "_kCFProxyAutoConfigurationJavaScriptKey",
        "kCFProxyAutoConfigurationJavaScriptKey",
    ),
    ("_kCFProxyHostNameKey", "kCFProxyHostNameKey"),
    ("_kCFProxyPortNumberKey", "kCFProxyPortNumberKey"),
    ("_kCFProxyUsernameKey", "kCFProxyUsernameKey"),
    ("_kCFProxyPasswordKey", "kCFProxyPasswordKey"),
];

fn key(text: &str) -> CFTypeRef {
    corefoundation::base::interned_name(text) as CFTypeRef
}

/// `CFDictionaryRef CFNetworkCopySystemProxySettings(void)`: an empty
/// dictionary, which is what no HTTP, HTTPS, SOCKS or auto-configuration
/// proxy looks like.
pub extern "C" fn CFNetworkCopySystemProxySettings() -> CFTypeRef {
    corefoundation::collections::new_dictionary(
        &[],
        Policy::from_key_callbacks(),
        Policy::from_type_callbacks(),
        false,
    )
}

/// `CFArrayRef CFNetworkCopyProxiesForURL(CFURLRef, CFDictionaryRef)`: one
/// entry, a direct connection.
pub extern "C" fn CFNetworkCopyProxiesForURL(_url: CFTypeRef, _settings: CFTypeRef) -> CFTypeRef {
    let direct = corefoundation::collections::new_dictionary(
        &[(key("kCFProxyTypeKey"), key("kCFProxyTypeNone"))],
        Policy::from_key_callbacks(),
        Policy::from_type_callbacks(),
        false,
    );
    let proxies =
        corefoundation::collections::new_array(&[direct], Policy::from_type_callbacks(), false);
    // SAFETY: the array took its own reference.
    unsafe { corefoundation::api::CFRelease(direct) };
    proxies
}

exports::exports! {
    functions {
        "CFNetworkCopySystemProxySettings" => CFNetworkCopySystemProxySettings,
        "CFNetworkCopyProxiesForURL" => CFNetworkCopyProxiesForURL,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_url_is_reached_directly() {
        crate::bootstrap();
        let proxies = CFNetworkCopyProxiesForURL(std::ptr::null_mut(), std::ptr::null_mut());
        let list = unsafe { corefoundation::collections::array(proxies) }.expect("an array");
        assert_eq!(list.values.len(), 1);
        let entry = unsafe { corefoundation::collections::dictionary(list.values[0]) }
            .expect("a dictionary");
        let at = entry.position(key("kCFProxyTypeKey")).expect("a type");
        assert_eq!(entry.entries[at].1, key("kCFProxyTypeNone"));
    }
}
