use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::DeriveInput;

/// Expands the `#[derive(Encode)]` macro, implementing the `Encode` trait
/// for a `Send + Copy + 'static + Display` type using raw memory copy for encoding
/// and `Display` formatting for decoding.
pub(crate) fn expand_derive_encode(input: DeriveInput) -> TokenStream2 {
    let mut crate_path: syn::Path = default_crate_path();
    for attr in &input.attrs {
        if !attr.path().is_ident("zerolog") {
            continue;
        }
        let result = attr.parse_nested_meta(|meta| {
            if !meta.path.is_ident("crate") {
                return Err(meta.error("expected `crate`"));
            }
            let value: syn::LitStr = meta.value()?.parse()?;
            crate_path = value.parse()?;
            Ok(())
        });
        if let Err(error) = result {
            return error.to_compile_error();
        }
    }
    let name = input.ident;
    let mut generics = input.generics;
    generics.make_where_clause().predicates.push(syn::parse_quote!(
        Self: ::core::marker::Send + ::core::marker::Copy + 'static + ::std::fmt::Display
    ));
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    quote! {
        impl #impl_generics #crate_path::Encode for #name #ty_generics #where_clause {
            const ENCODED_SIZE_HINT: usize = core::mem::size_of::<Self>();

            #[inline(always)]
            fn encoded_size(&self) -> usize {
                core::mem::size_of::<Self>()
            }

            #[inline(always)]
            unsafe fn encode(&self, buffer: *mut u8, offset: &mut usize) {
                unsafe {
                    buffer.add(*offset).cast::<Self>().write_unaligned(*self);
                }
                *offset += core::mem::size_of::<Self>();
            }

            #[inline(always)]
            unsafe fn encode_at_offset(&self, buffer: *mut u8, offset: usize) {
                unsafe {
                    buffer.add(offset).cast::<Self>().write_unaligned(*self);
                }
            }

            #[inline]
            unsafe fn decode_and_format(
                buffer: *const u8,
                offset: &mut usize,
                output: &mut Vec<u8>,
                format_spec: &str,
            ) {
                let value = unsafe { buffer.add(*offset).cast::<Self>().read_unaligned() };
                *offset += core::mem::size_of::<Self>();
                if format_spec.is_empty() {
                    #crate_path::append_display_const_default::<
                        Self,
                        b' ',
                        { #crate_path::FORMAT_ALIGN_NONE },
                        0,
                        { #crate_path::FORMAT_PRECISION_NONE },
                        false,
                    >(output, value);
                } else {
                    #crate_path::append_display_with_spec(output, value, format_spec);
                }
            }
        }

        unsafe impl #impl_generics #crate_path::EncodeFast for #name #ty_generics #where_clause {
            #[inline]
            unsafe fn decode_and_append_fast(
                buffer: *const u8,
                offset: &mut usize,
                output: &mut Vec<u8>,
            ) {
                let value = unsafe { buffer.add(*offset).cast::<Self>().read_unaligned() };
                *offset += core::mem::size_of::<Self>();
                #crate_path::append_display_const_default::<
                    Self,
                    b' ',
                    { #crate_path::FORMAT_ALIGN_NONE },
                    0,
                    { #crate_path::FORMAT_PRECISION_NONE },
                    false,
                >(output, value);
            }
        }
    }
}

pub(crate) fn default_crate_path() -> syn::Path {
    if std::env::var_os("CARGO_PKG_NAME").as_deref() == Some(std::ffi::OsStr::new("zerolog-rs")) {
        return syn::parse_quote!(::zerolog);
    }
    if let Some(manifest_dir) = std::env::var_os("CARGO_MANIFEST_DIR")
        && let Some(alias) = find_renamed_zerolog_in_dir(std::path::Path::new(&manifest_dir))
        && let Ok(ident) = syn::parse_str::<syn::Ident>(&alias)
    {
        return syn::parse_quote!(::#ident);
    }
    syn::parse_quote!(::zerolog)
}

fn find_renamed_zerolog_in_dir(manifest_dir: &std::path::Path) -> Option<String> {
    let manifest_path = manifest_dir.join("Cargo.toml");
    let contents = std::fs::read_to_string(&manifest_path).ok()?;
    let workspace_contents = find_workspace_manifest(manifest_dir);
    detect_renamed_zerolog_dep(&contents, workspace_contents.as_deref())
}

fn find_workspace_manifest(manifest_dir: &std::path::Path) -> Option<String> {
    let mut current = manifest_dir.parent();
    while let Some(dir) = current {
        let candidate = dir.join("Cargo.toml");
        if let Ok(contents) = std::fs::read_to_string(&candidate)
            && contents.contains("[workspace")
        {
            return Some(contents);
        }
        current = dir.parent();
    }
    None
}

enum TomlSection {
    DepList,
    DepTable(String),
    Other,
}

fn classify_section(header: &str) -> TomlSection {
    let h = header.trim();
    if matches!(
        h,
        "dependencies" | "dev-dependencies" | "build-dependencies" | "workspace.dependencies"
    ) || h.ends_with(".dependencies")
        || h.ends_with(".dev-dependencies")
        || h.ends_with(".build-dependencies")
    {
        return TomlSection::DepList;
    }
    for prefix in [
        "dependencies.",
        "dev-dependencies.",
        "build-dependencies.",
        "workspace.dependencies.",
        ".dependencies.",
        ".dev-dependencies.",
        ".build-dependencies.",
    ] {
        let rest = if prefix.starts_with('.') {
            h.find(prefix).map(|idx| &h[idx + prefix.len()..])
        } else {
            h.strip_prefix(prefix)
        };
        if let Some(alias) = rest {
            let alias = alias.trim().trim_matches('"').trim_matches('\'');
            if !alias.is_empty() && !alias.contains('.') {
                return TomlSection::DepTable(alias.to_string());
            }
        }
    }
    TomlSection::Other
}

fn strip_toml_comment(line: &str) -> &str {
    let mut in_double = false;
    let mut in_single = false;
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if in_double => {
                i += 2;
                continue;
            }
            b'"' if !in_single => in_double = !in_double,
            b'\'' if !in_double => in_single = !in_single,
            b'#' if !in_double && !in_single => return &line[..i],
            _ => {}
        }
        i += 1;
    }
    line
}

fn unquote(value: &str) -> &str {
    let trimmed = value.trim();
    if (trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2)
        || (trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2)
    {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    }
}

fn inline_table_has_kv(rhs: &str, expected_key: &str, expected_val: &str) -> bool {
    let trimmed = rhs.trim();
    let Some(inner) = trimmed.strip_prefix('{').and_then(|s| s.strip_suffix('}')) else {
        return false;
    };
    for part in inner.split(',') {
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        if unquote(k) == expected_key && unquote(v) == expected_val {
            return true;
        }
    }
    false
}

fn workspace_defines_alias_as_zerolog_rs(workspace_toml: Option<&str>, alias: &str) -> bool {
    let Some(ws) = workspace_toml else {
        return false;
    };
    detect_renamed_zerolog_dep(ws, None).as_deref() == Some(alias)
}

fn detect_renamed_zerolog_dep(contents: &str, workspace_toml: Option<&str>) -> Option<String> {
    let mut section = TomlSection::Other;
    let mut pending_inline: Option<(String, String)> = None;

    for raw_line in contents.lines() {
        let line = strip_toml_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }
        if let Some((alias, mut acc)) = pending_inline.take() {
            acc.push(' ');
            acc.push_str(line);
            if acc.contains('}') {
                if alias != "zerolog"
                    && (inline_table_has_kv(&acc, "package", "zerolog-rs")
                        || (inline_table_has_kv(&acc, "workspace", "true")
                            && workspace_defines_alias_as_zerolog_rs(workspace_toml, &alias)))
                {
                    return Some(alias);
                }
            } else {
                pending_inline = Some((alias, acc));
            }
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') && !line.starts_with("[[") {
            section = classify_section(&line[1..line.len() - 1]);
            continue;
        }
        let Some((lhs, rhs)) = line.split_once('=') else {
            continue;
        };
        let key = unquote(lhs);
        let rhs = rhs.trim();

        match &section {
            TomlSection::Other => {}
            TomlSection::DepTable(alias) => {
                if alias != "zerolog" {
                    if key == "package" && unquote(rhs) == "zerolog-rs" {
                        return Some(alias.clone());
                    }
                    if key == "workspace"
                        && unquote(rhs) == "true"
                        && workspace_defines_alias_as_zerolog_rs(workspace_toml, alias)
                    {
                        return Some(alias.clone());
                    }
                }
            }
            TomlSection::DepList => {
                if let Some((alias, subkey)) = key.split_once('.') {
                    let alias = unquote(alias);
                    let subkey = unquote(subkey);
                    if alias != "zerolog" {
                        if subkey == "package" && unquote(rhs) == "zerolog-rs" {
                            return Some(alias.to_string());
                        }
                        if subkey == "workspace"
                            && unquote(rhs) == "true"
                            && workspace_defines_alias_as_zerolog_rs(workspace_toml, alias)
                        {
                            return Some(alias.to_string());
                        }
                    }
                } else if rhs.starts_with('{') {
                    if rhs.contains('}') {
                        if key != "zerolog"
                            && (inline_table_has_kv(rhs, "package", "zerolog-rs")
                                || (inline_table_has_kv(rhs, "workspace", "true")
                                    && workspace_defines_alias_as_zerolog_rs(workspace_toml, key)))
                        {
                            return Some(key.to_string());
                        }
                    } else {
                        pending_inline = Some((key.to_string(), rhs.to_string()));
                    }
                }
            }
        }
    }
    None
}
