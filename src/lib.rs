#![doc = include_str!("../README.md")]
#![cfg_attr(feature = "nightly", feature(proc_macro_span))]
mod error;
mod exports;
mod files;
mod imports;
mod module;
mod result;
mod source;

use core::panic;
use std::{fs::File, io::Read, path::PathBuf};

use walkdir::{WalkDir, DirEntry};
use files::AbsoluteRustFilePathBuf;
use quote::ToTokens;
use source::Sourcecode;
use syn::token::Brace;

fn iter_files_with_ext<'a>(dir: &'a PathBuf, ext: &'a str) -> impl Iterator<Item = DirEntry> + 'a {
    let target_dir = std::env::var("CARGO_TARGET_DIR").ok().map(|string| PathBuf::from(string));
    WalkDir::new(dir)
        .into_iter()
        .filter_entry(move |entry| {
            target_dir.as_ref().map_or(true, |target_dir| !entry.path().starts_with(target_dir))
        })
        .filter_map(|result| result.ok())
        .filter(move |result| {
            return result.file_type().is_file() && result.path().extension().unwrap_or_default() == ext;
        })
}

// Hacky polyfill for `proc_macro::Span::source_file`
fn find_me(crate_root: &PathBuf, pattern: &str) -> Option<PathBuf> {
    let mut options = Vec::new();

    for entry in iter_files_with_ext(crate_root, "rs") {
        let path = entry.path();
        if let Ok(mut f) = File::open(&path) {
            let mut contents = String::new();
            f.read_to_string(&mut contents).ok();
            if contents.contains(pattern) {
                options.push(path.to_owned());
            }
        }
    }

    match options.as_slice() {
        [] => None,
        [v] => Some(v.clone()),
        _ => panic!(
            "found more than one contender for macro invocation location. \
            This won't be an issue once `proc_macro_span` is stabalized, \
            but until then each instance of the `include_wgsl_oil` \
            must be present in the source text, and each must have a unique argument. \
            found locations: {:?}",
            options
                .into_iter()
                .map(|path| format!("`{}`", path.display()))
                .collect::<Vec<String>>()
        ),
    }
}

#[proc_macro_attribute]
pub fn include_wgsl_oil(
    path: proc_macro::TokenStream,
    module: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    // Parse module definitions and error if it contains anything
    let mut module = syn::parse_macro_input!(module as syn::ItemMod);
    if let Some(content) = &mut module.content {
        if !content.1.is_empty() {
            let item = syn::parse_quote_spanned! {content.0.span=>
                compile_error!(
                    "`include_wgsl_oil` expects an empty module into which to inject the shader objects, \
                    but found a module body - try removing everything within the curly braces `{ ... }`.");
            };

            module.content = Some((Brace::default(), vec![item]));
        }
    } else {
        module.content = Some((Brace::default(), vec![]));
    }
    module.semi = None;

    let requested_path = syn::parse_macro_input!(path as syn::LitStr);
    let requested_path = requested_path.value();


    let crate_root = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("proc macros should be run using cargo")
    );

    let mut invocation_path: Option<AbsoluteRustFilePathBuf> = None;
    #[cfg(feature = "nightly")] {
        let span = proc_macro::Span::call_site();
        let source_file = proc_macro::Span::source_file(&span);
        match source_file.path().to_str() {
            Some("") | None => {
                // Fall back to the grep method if for some reason the source file is empty
                invocation_path = Some(match find_me(&crate_root, &format!("\"{}\"", requested_path)) {
                    Some(invocation_path) => AbsoluteRustFilePathBuf::new(invocation_path),
                    None => {
                        panic!(
                            "could not find invocation point - maybe it was in a macro? This won't be an issue once \
                            `proc_macro_span` is stabalized, but until then each instance of the `include_wgsl_oil` \
                            must be present in the source text, and each must have a unique argument."
                        )
                    }
                })
            },
            Some(path) => {
                let workspace_root = crate_root.ancestors()
                    .find(|p| p.join("Cargo.lock").exists())
                    .expect("Unable to find workspace root");
                let path = PathBuf::from(workspace_root).join(path);
                invocation_path = Some(AbsoluteRustFilePathBuf::new(path));
            }
        }
    }
    #[cfg(not(feature = "nightly"))] {
        invocation_path = Some(match find_me(&crate_root, &format!("\"{}\"", requested_path)) {
            Some(invocation_path) => AbsoluteRustFilePathBuf::new(invocation_path),
            None => {
                panic!(
                    "could not find invocation point - maybe it was in a macro? This won't be an issue once \
                    `proc_macro_span` is stabalized, but until then each instance of the `include_wgsl_oil` \
                    must be present in the source text, and each must have a unique argument."
                )
            }
        })
    }

    let sourcecode = Sourcecode::new(invocation_path.unwrap(), requested_path);

    let mut result = sourcecode.complete();

    result.validate();

    // Inject items
    module
        .content
        .as_mut()
        .expect("set to some at start")
        .1
        .append(&mut result.items());

    module.to_token_stream().into()
}
