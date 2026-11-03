pub mod codesign;
pub mod keychain;
pub mod trust;

exports::group! {
    codesign,
    keychain,
    trust,
}

/// The `kSec*` key globals, each a cell holding an `NSString *`.
pub fn data_symbols() -> Vec<(String, u64)> {
    foundation::bootstrap();
    let mut symbols: Vec<_> = keychain::KEYS
        .iter()
        .chain(trust::KEYS)
        .map(|(symbol, text)| {
            (
                (*symbol).to_string(),
                corefoundation::base::global_cell(
                    symbol,
                    foundation::notification::name_object(text),
                ),
            )
        })
        .collect();
    // `const SecRandomRef kSecRandomDefault = NULL`: a word the guest reads.
    let random = "_kSecRandomDefault";
    symbols.push((
        random.to_string(),
        corefoundation::base::global_cell(random, 0),
    ));
    symbols
}
