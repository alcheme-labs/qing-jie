use std::fmt::Display;

use objc2::rc::Retained;
use objc2_foundation::{NSBundle, NSString};

pub(crate) fn text(english: &str) -> Retained<NSString> {
    let key = NSString::from_str(english);
    NSBundle::mainBundle().localizedStringForKey_value_table(&key, Some(&key), None)
}

pub(crate) fn string(english: &str) -> String {
    text(english).to_string()
}

// Named values let translations change word order without duplicating templates in Rust.
pub(crate) fn format(english: &str, args: &[(&str, &dyn Display)]) -> String {
    let template = string(english);
    let mut result = String::with_capacity(template.len());
    let mut tail = template.as_str();
    while let Some((before, after_open)) = tail.split_once('{') {
        let Some((name, after_close)) = after_open.split_once('}') else { break; };
        result.push_str(before);
        if let Some((_, value)) = args.iter().find(|(key, _)| *key == name) {
            use std::fmt::Write;
            let _ = write!(result, "{value}");
        } else {
            result.push('{');
            result.push_str(name);
            result.push('}');
        }
        tail = after_close;
    }
    result.push_str(tail);
    result
}
