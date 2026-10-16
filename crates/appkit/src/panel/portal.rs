use std::path::PathBuf;

use ashpd::desktop::ResponseError;
use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};
use ashpd::{Error as PortalError, PortalError as PortalDbusError};
use objc::object::Id;
use url::Url;

use super::{PanelState, PortalOutcome, initial_directory, recognized_extensions};

pub(super) fn show(panel: &PanelState) -> PortalOutcome {
    if !supports_selection_options(panel) {
        eprintln!(
            "rosemary: appkit: XDG file chooser cannot represent this panel's selection options; using the built-in chooser"
        );
        return PortalOutcome::Unavailable;
    }
    let Some(filters) = panel_filters(panel) else {
        eprintln!(
            "rosemary: appkit: XDG file chooser cannot represent this panel's accessory view; using the built-in chooser"
        );
        return PortalOutcome::Unavailable;
    };
    eprintln!(
        "rosemary: appkit: requesting XDG file chooser ({})",
        if panel.save { "SaveFile" } else { "OpenFile" }
    );
    async_io::block_on(show_async(panel, filters))
}

fn supports_selection_options(panel: &PanelState) -> bool {
    if panel.shows_hidden_files {
        return false;
    }
    if panel.save {
        return !panel.multiple;
    }
    panel.can_choose_files ^ panel.can_choose_directories
}

struct PortalFilters {
    values: Vec<FileFilter>,
    current: Option<FileFilter>,
}

fn panel_filters(panel: &PanelState) -> Option<PortalFilters> {
    if panel.accessory_view != 0 {
        return accessory_filters(panel.accessory_view as Id);
    }

    let values = portal_filters(&panel.allowed_file_types);
    Some(PortalFilters {
        current: values.first().cloned(),
        values,
    })
}

fn accessory_filters(accessory: Id) -> Option<PortalFilters> {
    if !is_kind_of(accessory, "NSView") {
        return None;
    }
    let subviews = send_object(accessory, "subviews")?;
    let subviews = unsafe { corefoundation::collections::array(subviews as _) }?;
    let mut kinds = Vec::with_capacity(subviews.values.len());
    let mut popup = None;
    for subview in &subviews.values {
        let subview = *subview as Id;
        if is_kind_of(subview, "NSPopUpButton") {
            if popup.replace(subview).is_some() {
                return None;
            }
            kinds.push("NSPopUpButton".to_owned());
        } else if is_kind_of(subview, "NSTextField") {
            kinds.push("NSTextField".to_owned());
        } else {
            return None;
        }
    }
    if kinds.len() != 2 || popup.is_none() {
        return None;
    }

    let popup = popup?;
    let titles = send_object(popup, "itemTitles")?;
    let titles = unsafe { corefoundation::collections::array(titles as _) }?;
    let labels = titles
        .values
        .iter()
        .map(|title| object_string(*title as Id))
        .collect::<Option<Vec<_>>>()?;
    let selected = object_string(send_object(popup, "titleOfSelectedItem")?);
    qt_accessory_filters(&kinds, &labels, selected.as_deref())
}

fn is_kind_of(object: Id, expected: &str) -> bool {
    if object.is_null() {
        return false;
    }
    unsafe { objc::object::class_of(object) }
        .is_some_and(|class| class.chain().any(|class| class.name_str() == expected))
}

fn send_object(receiver: Id, selector: &str) -> Option<Id> {
    if receiver.is_null() {
        return None;
    }
    let selector = objc::sel::get(selector);
    let class = unsafe { objc::object::class_of(receiver) }?;
    if !class.chain().any(|class| class.method(selector).is_some()) {
        return None;
    }
    let send: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    Some(unsafe { send(receiver, selector) })
}

fn object_string(object: Id) -> Option<String> {
    if object.is_null() {
        return None;
    }
    Some(String::from_utf8_lossy(&unsafe { foundation::string::bytes(object) }).into_owned())
}

fn qt_accessory_filters(
    subview_kinds: &[String],
    labels: &[String],
    selected: Option<&str>,
) -> Option<PortalFilters> {
    if subview_kinds.len() != 2
        || !subview_kinds.iter().any(|kind| kind == "NSPopUpButton")
        || !subview_kinds.iter().any(|kind| kind == "NSTextField")
        || labels.is_empty()
    {
        return None;
    }
    let values = labels
        .iter()
        .map(|label| qt_name_filter(label))
        .collect::<Option<Vec<_>>>()?;
    let current = selected.and_then(|selected| {
        values
            .iter()
            .find(|filter| filter.label() == selected)
            .cloned()
    });
    Some(PortalFilters { values, current })
}

fn qt_name_filter(label: &str) -> Option<FileFilter> {
    let patterns = qt_glob_patterns(label)?;
    let mut filter = FileFilter::new(&label.replace('\0', " "));
    for pattern in patterns {
        filter = filter.glob(&pattern);
    }
    Some(filter)
}

fn qt_glob_patterns(label: &str) -> Option<Vec<String>> {
    let bytes = label.as_bytes();
    let mut patterns = Vec::new();
    let mut index = 0;
    while index + 1 < bytes.len() {
        if bytes[index] != b'*' || bytes[index + 1] != b'.' {
            index += 1;
            continue;
        }
        index += 2;
        if bytes.get(index) == Some(&b'*') {
            patterns.push("*.*".to_owned());
            index += 1;
            continue;
        }
        let start = index;
        while bytes.get(index).is_some_and(|byte| {
            byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'-' | b'+' | b'.')
        }) {
            index += 1;
        }
        if start == index {
            return None;
        }
        if bytes.get(index).is_some_and(|byte| {
            !byte.is_ascii_whitespace() && !matches!(*byte, b',' | b';' | b')' | b']')
        }) {
            return None;
        }
        let extension = std::str::from_utf8(&bytes[start..index]).ok()?;
        patterns.push(format!("*.{extension}"));
    }
    if patterns.is_empty() && label.contains("(*)") {
        patterns.push("*".to_owned());
    }
    (!patterns.is_empty()).then_some(patterns)
}

async fn show_async(panel: &PanelState, filters: PortalFilters) -> PortalOutcome {
    let title = panel
        .title
        .as_deref()
        .unwrap_or(if panel.save { "Save File" } else { "Open File" })
        .replace('\0', " ");
    let accept_label = panel
        .prompt
        .as_deref()
        .unwrap_or(if panel.save { "Save" } else { "Open" })
        .replace('\0', " ");
    let PortalFilters { values, current } = filters;

    let request = if panel.save {
        let mut request = SelectedFiles::save_file()
            .title(title.as_str())
            .accept_label(accept_label.as_str())
            .modal(true);
        if let Some(filename) = panel.filename.as_deref() {
            let filename = filename.replace('\0', " ");
            request = request.current_name(filename.as_str());
        }
        request = match request.current_folder(initial_directory(panel)) {
            Ok(request) => request,
            Err(error) => return failed(error.into()),
        };
        request.filters(values).current_filter(current).send().await
    } else {
        let mut request = SelectedFiles::open_file()
            .title(title.as_str())
            .accept_label(accept_label.as_str())
            .modal(true)
            .multiple(panel.multiple)
            .directory(panel.can_choose_directories);
        request = match request.current_folder(initial_directory(panel)) {
            Ok(request) => request,
            Err(error) => return failed(error.into()),
        };
        request.filters(values).current_filter(current).send().await
    };

    let request = match request {
        Ok(request) => request,
        Err(error) => return failed(error),
    };
    let selected = match request.response() {
        Ok(selected) => selected,
        Err(error) => return failed(error),
    };
    let Some(paths) = selected
        .uris()
        .iter()
        .map(|uri| local_file_path(uri.as_str()))
        .collect::<Option<Vec<_>>>()
        .filter(|paths| !paths.is_empty())
    else {
        eprintln!("rosemary: appkit: XDG file chooser returned no local file paths");
        return PortalOutcome::Unavailable;
    };
    PortalOutcome::Selected(paths)
}

fn portal_filters(types: &[String]) -> Vec<FileFilter> {
    let extensions = recognized_extensions(types);
    if extensions.is_empty() {
        return Vec::new();
    }
    let patterns: Vec<String> = extensions
        .iter()
        .map(|extension| format!("*.{extension}"))
        .collect();
    let label = format!("Supported files ({})", patterns.join(", "));
    let mut filter = FileFilter::new(&label);
    for pattern in patterns {
        filter = filter.glob(&pattern);
    }
    vec![filter]
}

fn local_file_path(uri: &str) -> Option<PathBuf> {
    let scheme_separator = uri.find("://")?;
    if !uri[..scheme_separator].eq_ignore_ascii_case("file") {
        return None;
    }
    let uri = Url::parse(uri).ok()?;
    if uri.scheme() != "file" {
        return None;
    }
    let path = uri.to_file_path().ok()?;
    path.is_absolute().then_some(path)
}

fn failed(error: PortalError) -> PortalOutcome {
    match error {
        PortalError::Response(ResponseError::Cancelled)
        | PortalError::Portal(PortalDbusError::Cancelled(_)) => PortalOutcome::Cancelled,
        error => {
            eprintln!(
                "rosemary: appkit: XDG file chooser unavailable or failed ({error}); using the built-in chooser"
            );
            PortalOutcome::Unavailable
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uris_decode_spaces_and_unicode() {
        assert_eq!(
            local_file_path("file:///tmp/Studio%20Projects/%E2%98%83.rbxl"),
            Some(PathBuf::from("/tmp/Studio Projects/☃.rbxl"))
        );
    }

    #[test]
    fn non_file_uris_are_rejected() {
        assert_eq!(local_file_path("https://example.com/place.rbxl"), None);
        assert_eq!(local_file_path("file:relative/place.rbxl"), None);
        assert_eq!(local_file_path("file://remote/share/place.rbxl"), None);
    }

    #[test]
    fn portal_filter_combines_allowed_extensions() {
        let types = vec!["rbxl".to_owned(), ".rbxm".to_owned()];
        let filters = portal_filters(&types);
        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].label(), "Supported files (*.rbxl, *.rbxm)");
        assert_eq!(filters[0].pattern_filters(), ["*.rbxl", "*.rbxm"]);
    }

    #[test]
    fn panels_with_unsupported_selection_options_use_the_internal_chooser() {
        let open = PanelState {
            can_choose_files: true,
            ..PanelState::default()
        };
        assert!(supports_selection_options(&open));

        let save = PanelState {
            save: true,
            ..PanelState::default()
        };
        assert!(supports_selection_options(&save));

        let mixed = PanelState {
            can_choose_files: true,
            can_choose_directories: true,
            ..PanelState::default()
        };
        assert!(!supports_selection_options(&mixed));
        assert!(!supports_selection_options(&PanelState {
            shows_hidden_files: true,
            can_choose_files: true,
            ..PanelState::default()
        }));
    }

    #[test]
    fn qt_name_filter_accessory_maps_filter_labels_and_selection() {
        let kinds = vec!["NSPopUpButton".to_owned(), "NSTextField".to_owned()];
        let labels = vec![
            "Studio Places (*.rbxl *.rbxlx)".to_owned(),
            "Studio Models (*.rbxm)".to_owned(),
            "All Files (*)".to_owned(),
        ];
        let filters = qt_accessory_filters(&kinds, &labels, Some("Studio Models (*.rbxm)"))
            .expect("Qt's file-type accessory should map to portal filters");

        assert_eq!(
            filters
                .values
                .iter()
                .map(FileFilter::label)
                .collect::<Vec<_>>(),
            [
                "Studio Places (*.rbxl *.rbxlx)",
                "Studio Models (*.rbxm)",
                "All Files (*)"
            ]
        );
        assert_eq!(filters.values[0].pattern_filters(), ["*.rbxl", "*.rbxlx"]);
        assert_eq!(filters.values[2].pattern_filters(), ["*"]);
        assert_eq!(
            filters.current.as_ref().map(FileFilter::label),
            Some("Studio Models (*.rbxm)")
        );
    }

    #[test]
    fn custom_accessories_and_unrecognized_name_filters_still_fall_back() {
        let qt_kinds = vec!["NSPopUpButton".to_owned(), "NSTextField".to_owned()];
        assert!(
            qt_accessory_filters(
                &["NSPopUpButton".to_owned(), "NSButton".to_owned()],
                &["Studio Places (*.rbxl)".to_owned()],
                None
            )
            .is_none()
        );
        assert!(
            qt_accessory_filters(&qt_kinds, &["Studio Files (*.rbxl?)".to_owned()], None).is_none()
        );
        assert_eq!(
            qt_glob_patterns("All Files (*.*)"),
            Some(vec!["*.*".to_owned()])
        );
    }

    #[test]
    fn portal_cancellation_is_distinct_from_portal_failure() {
        assert!(matches!(
            failed(PortalError::Response(ResponseError::Cancelled)),
            PortalOutcome::Cancelled
        ));
        assert!(matches!(
            failed(PortalError::Portal(PortalDbusError::Failed(
                "backend unavailable".into()
            ))),
            PortalOutcome::Unavailable
        ));
    }
}
