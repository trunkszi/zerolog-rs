use proc_macro2::{Literal, Span, TokenStream as TokenStream2};
use quote::quote;
use syn::{LitStr, Result};

/// Result of parsing a format string into its literal segments, format
/// specifiers, and placeholder count.
pub(crate) struct ParsedFormat {
    /// Literal text segments between placeholders (length = placeholder_count + 1).
    pub(crate) literal_segments: Vec<String>,
    /// Format specifier for each placeholder (e.g., `":x"` from `{:x}`).
    pub(crate) specs: Vec<String>,
    pub(crate) arguments: Vec<FormatArgument>,
    /// Total number of `{}` placeholders found.
    pub(crate) placeholder_count: usize,
}

/// A placeholder selects a positional argument or captures a local identifier.
pub(crate) enum FormatArgument {
    Position(usize),
    Capture(syn::Ident),
}

/// Parses a format string literal into [`ParsedFormat`], extracting literal
/// segments and per-placeholder format specifiers.
///
/// Returns an error for unclosed `{` or unmatched `}`.
pub(crate) fn parse_format_string(format_string: &LitStr) -> Result<ParsedFormat> {
    let value = format_string.value();
    let mut literal = String::new();
    let mut literal_segments = Vec::new();
    let mut specs = Vec::new();
    let mut placeholder_count = 0usize;
    let mut arguments = Vec::new();
    let mut implicit = 0usize;
    let mut explicit = false;
    let mut chars = value.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '{' => {
                if chars.peek() == Some(&'{') {
                    chars.next();
                    literal.push('{');
                    continue;
                }

                let mut spec = String::new();
                loop {
                    let Some(next) = chars.next() else {
                        return Err(syn::Error::new(format_string.span(), "unclosed `{`"));
                    };

                    if next == '}' {
                        break;
                    }

                    spec.push(next);
                }

                let (arg, spec) = match spec.split_once(':') {
                    Some((arg, spec)) => (arg, format!(":{spec}")),
                    None => (spec.as_str(), String::new()),
                };
                let arg = if arg.is_empty() {
                    if explicit {
                        return Err(syn::Error::new(
                            format_string.span(),
                            "cannot mix implicit and explicit positional arguments",
                        ));
                    }
                    let next = implicit;
                    implicit += 1;
                    FormatArgument::Position(next)
                } else if arg.bytes().all(|b| b.is_ascii_digit()) {
                    if implicit != 0 {
                        return Err(syn::Error::new(
                            format_string.span(),
                            "cannot mix implicit and explicit positional arguments",
                        ));
                    }
                    explicit = true;
                    FormatArgument::Position(arg.parse().map_err(|_| {
                        syn::Error::new(format_string.span(), "argument index exceeds usize")
                    })?)
                } else {
                    let mut id: syn::Ident = syn::parse_str(arg).map_err(|_| {
                        syn::Error::new(
                            format_string.span(),
                            "expected an argument index or identifier",
                        )
                    })?;
                    id.set_span(format_string.span());
                    FormatArgument::Capture(id)
                };
                if !spec.is_empty() {
                    let parsed = parse_const_format_spec(&spec).ok_or_else(|| {
                        syn::Error::new(
                            format_string.span(),
                            "invalid or overflowing format width/precision",
                        )
                    })?;
                    if !matches!(parsed.ty, None | Some(b'x' | b'X' | b'b' | b'o' | b'e' | b'E')) {
                        return Err(syn::Error::new(
                            format_string.span(),
                            "unsupported format trait; use Display, x/X, b, o, or e/E",
                        ));
                    }
                }
                arguments.push(arg);
                literal_segments.push(std::mem::take(&mut literal));
                specs.push(spec);
                placeholder_count += 1;
            }
            '}' => {
                if chars.peek() == Some(&'}') {
                    chars.next();
                    literal.push('}');
                } else {
                    return Err(syn::Error::new(format_string.span(), "unmatched `}`"));
                }
            }
            _ => literal.push(ch),
        }
    }

    literal_segments.push(literal);

    Ok(ParsedFormat { literal_segments, specs, arguments, placeholder_count })
}

/// Defines the typed decoder referenced by the call site's constant metadata.
pub(crate) fn build_decoder_factory(
    fragments: &ParsedFormat,
    type_params: &[syn::Ident],
    encode_trait: &TokenStream2,
    fast_path: bool,
    crate_path: &syn::Path,
) -> TokenStream2 {
    let body = build_decoder_body(fragments, fast_path, type_params, crate_path);
    let bounds = type_params.iter().map(|t| quote!(#t: #encode_trait));
    let generics = if type_params.is_empty() { quote!() } else { quote!(<#(#bounds),*>) };
    quote! {
        unsafe fn __zerolog_decode #generics(buffer: *const u8, output: &mut Vec<u8>) {
            let _ = buffer;
            #body
        }
    }
}

/// Generates the body of the decoder function that reads encoded values from
/// a raw buffer and appends their formatted representations to `output`.
pub(crate) fn build_decoder_body(
    fragments: &ParsedFormat,
    fast_path: bool,
    type_params: &[syn::Ident],
    crate_path: &syn::Path,
) -> TokenStream2 {
    let mut sequential_statements = Vec::new();

    for (index, literal_segment) in fragments.literal_segments.iter().enumerate() {
        let literal = Literal::byte_string(literal_segment.as_bytes());
        sequential_statements.push(quote!(output.extend_from_slice(#literal);));

        if let Some(type_param) = type_params.get(index) {
            sequential_statements.push(build_decode_statement(
                type_param,
                &fragments.specs[index],
                fast_path,
                None,
                crate_path,
            ));
        }
    }

    if type_params.is_empty() {
        quote! {
            #(#sequential_statements)*
        }
    } else {
        quote! {
            let mut __zerolog_offset = 0usize;
            #(#sequential_statements)*
        }
    }
}

fn build_decode_statement(
    type_param: &syn::Ident,
    format_spec: &str,
    fast_path: bool,
    fixed_offset: Option<TokenStream2>,
    crate_path: &syn::Path,
) -> TokenStream2 {
    let fixed_offset_ref = fixed_offset.as_ref();

    if fast_path {
        return if let Some(offset) = fixed_offset_ref {
            quote! {
                unsafe {
                    <#type_param as #crate_path::EncodeFast>::decode_and_append_fast_at_offset(
                        buffer,
                        #offset,
                        output,
                    );
                }
            }
        } else {
            quote! {
                unsafe {
                    <#type_param as #crate_path::EncodeFast>::decode_and_append_fast(
                        buffer,
                        &mut __zerolog_offset,
                        output,
                    );
                }
            }
        };
    }

    let format_spec_literal = syn::LitStr::new(format_spec, Span::call_site());
    let specialized = match format_spec {
        ":x" => Some(if let Some(offset) = fixed_offset_ref {
            quote! {
                unsafe {
                    <#type_param as #crate_path::EncodeLowerHex>::decode_append_lower_hex_at_offset::<false>(
                        buffer,
                        #offset,
                        output,
                    );
                }
            }
        } else {
            quote! {
                unsafe {
                    <#type_param as #crate_path::EncodeLowerHex>::decode_append_lower_hex::<false>(
                        buffer,
                        &mut __zerolog_offset,
                        output,
                    );
                }
            }
        }),
        ":#x" => Some(if let Some(offset) = fixed_offset_ref {
            quote! {
                unsafe {
                    <#type_param as #crate_path::EncodeLowerHex>::decode_append_lower_hex_at_offset::<true>(
                        buffer,
                        #offset,
                        output,
                    );
                }
            }
        } else {
            quote! {
                unsafe {
                    <#type_param as #crate_path::EncodeLowerHex>::decode_append_lower_hex::<true>(
                        buffer,
                        &mut __zerolog_offset,
                        output,
                    );
                }
            }
        }),
        ":X" => Some(if let Some(offset) = fixed_offset_ref {
            quote! {
                unsafe {
                    <#type_param as #crate_path::EncodeUpperHex>::decode_append_upper_hex_at_offset::<false>(
                        buffer,
                        #offset,
                        output,
                    );
                }
            }
        } else {
            quote! {
                unsafe {
                    <#type_param as #crate_path::EncodeUpperHex>::decode_append_upper_hex::<false>(
                        buffer,
                        &mut __zerolog_offset,
                        output,
                    );
                }
            }
        }),
        ":#X" => Some(if let Some(offset) = fixed_offset_ref {
            quote! {
                unsafe {
                    <#type_param as #crate_path::EncodeUpperHex>::decode_append_upper_hex_at_offset::<true>(
                        buffer,
                        #offset,
                        output,
                    );
                }
            }
        } else {
            quote! {
                unsafe {
                    <#type_param as #crate_path::EncodeUpperHex>::decode_append_upper_hex::<true>(
                        buffer,
                        &mut __zerolog_offset,
                        output,
                    );
                }
            }
        }),
        ":b" => Some(if let Some(offset) = fixed_offset_ref {
            quote! {
                unsafe {
                    <#type_param as #crate_path::EncodeBinary>::decode_append_binary_at_offset::<false>(
                        buffer,
                        #offset,
                        output,
                    );
                }
            }
        } else {
            quote! {
                unsafe {
                    <#type_param as #crate_path::EncodeBinary>::decode_append_binary::<false>(
                        buffer,
                        &mut __zerolog_offset,
                        output,
                    );
                }
            }
        }),
        ":#b" => Some(if let Some(offset) = fixed_offset_ref {
            quote! {
                unsafe {
                    <#type_param as #crate_path::EncodeBinary>::decode_append_binary_at_offset::<true>(
                        buffer,
                        #offset,
                        output,
                    );
                }
            }
        } else {
            quote! {
                unsafe {
                    <#type_param as #crate_path::EncodeBinary>::decode_append_binary::<true>(
                        buffer,
                        &mut __zerolog_offset,
                        output,
                    );
                }
            }
        }),
        ":o" => Some(if let Some(offset) = fixed_offset_ref {
            quote! {
                unsafe {
                    <#type_param as #crate_path::EncodeOctal>::decode_append_octal_at_offset::<false>(
                        buffer,
                        #offset,
                        output,
                    );
                }
            }
        } else {
            quote! {
                unsafe {
                    <#type_param as #crate_path::EncodeOctal>::decode_append_octal::<false>(
                        buffer,
                        &mut __zerolog_offset,
                        output,
                    );
                }
            }
        }),
        ":#o" => Some(if let Some(offset) = fixed_offset_ref {
            quote! {
                unsafe {
                    <#type_param as #crate_path::EncodeOctal>::decode_append_octal_at_offset::<true>(
                        buffer,
                        #offset,
                        output,
                    );
                }
            }
        } else {
            quote! {
                unsafe {
                    <#type_param as #crate_path::EncodeOctal>::decode_append_octal::<true>(
                        buffer,
                        &mut __zerolog_offset,
                        output,
                    );
                }
            }
        }),
        _ => build_const_format_dispatch(type_param, format_spec, fixed_offset_ref, crate_path),
    };

    specialized.unwrap_or_else(|| {
        if let Some(offset) = fixed_offset_ref {
            quote! {
                unsafe {
                    <#type_param as #crate_path::Encode>::decode_and_format_at_offset(
                        buffer,
                        #offset,
                        output,
                        #format_spec_literal,
                    );
                }
            }
        } else {
            quote! {
                unsafe {
                    <#type_param as #crate_path::Encode>::decode_and_format(
                        buffer,
                        &mut __zerolog_offset,
                        output,
                        #format_spec_literal,
                    );
                }
            }
        }
    })
}

struct ConstFormatSpec {
    fill: u8,
    alignment: u8,
    width: usize,
    precision: Option<usize>,
    alternate: bool,
    zero_pad: bool,
    ty: Option<u8>,
}

fn build_const_format_dispatch(
    type_param: &syn::Ident,
    format_spec: &str,
    fixed_offset: Option<&TokenStream2>,
    crate_path: &syn::Path,
) -> Option<TokenStream2> {
    let parsed = parse_const_format_spec(format_spec)?;
    let fill = Literal::u8_unsuffixed(parsed.fill);
    let alignment = Literal::u8_unsuffixed(parsed.alignment);
    let width = Literal::usize_unsuffixed(parsed.width);
    let precision = parsed.precision.map(Literal::usize_unsuffixed);
    let precision = precision
        .map(|value| quote!({ #value }))
        .unwrap_or_else(|| quote!({ #crate_path::FORMAT_PRECISION_NONE }));
    let zero_pad = parsed.zero_pad;
    let alternate = parsed.alternate;

    match parsed.ty {
        None if !parsed.alternate => Some(if let Some(offset) = fixed_offset {
            quote! {
                unsafe {
                    <#type_param as #crate_path::EncodeConstDefaultFormat>::decode_append_default_format_at_offset::<
                        #fill,
                        #alignment,
                        #width,
                        #precision,
                        #zero_pad
                    >(
                        buffer,
                        #offset,
                        output,
                    );
                }
            }
        } else {
            quote! {
                unsafe {
                    <#type_param as #crate_path::EncodeConstDefaultFormat>::decode_append_default_format::<
                        #fill,
                        #alignment,
                        #width,
                        #precision,
                        #zero_pad
                    >(
                        buffer,
                        &mut __zerolog_offset,
                        output,
                    );
                }
            }
        }),
        Some(b'x') if parsed.precision.is_none() => Some(if let Some(offset) = fixed_offset {
            quote! {
                unsafe {
                    <#type_param as #crate_path::EncodeConstLowerHexFormat>::decode_append_lower_hex_format_at_offset::<
                        #fill,
                        #alignment,
                        #width,
                        #zero_pad,
                        #alternate
                    >(
                        buffer,
                        #offset,
                        output,
                    );
                }
            }
        } else {
            quote! {
                unsafe {
                    <#type_param as #crate_path::EncodeConstLowerHexFormat>::decode_append_lower_hex_format::<
                        #fill,
                        #alignment,
                        #width,
                        #zero_pad,
                        #alternate
                    >(
                        buffer,
                        &mut __zerolog_offset,
                        output,
                    );
                }
            }
        }),
        Some(b'X') if parsed.precision.is_none() => Some(if let Some(offset) = fixed_offset {
            quote! {
                unsafe {
                    <#type_param as #crate_path::EncodeConstUpperHexFormat>::decode_append_upper_hex_format_at_offset::<
                        #fill,
                        #alignment,
                        #width,
                        #zero_pad,
                        #alternate
                    >(
                        buffer,
                        #offset,
                        output,
                    );
                }
            }
        } else {
            quote! {
                unsafe {
                    <#type_param as #crate_path::EncodeConstUpperHexFormat>::decode_append_upper_hex_format::<
                        #fill,
                        #alignment,
                        #width,
                        #zero_pad,
                        #alternate
                    >(
                        buffer,
                        &mut __zerolog_offset,
                        output,
                    );
                }
            }
        }),
        Some(b'b') if parsed.precision.is_none() => Some(if let Some(offset) = fixed_offset {
            quote! {
                unsafe {
                    <#type_param as #crate_path::EncodeConstBinaryFormat>::decode_append_binary_format_at_offset::<
                        #fill,
                        #alignment,
                        #width,
                        #zero_pad,
                        #alternate
                    >(
                        buffer,
                        #offset,
                        output,
                    );
                }
            }
        } else {
            quote! {
                unsafe {
                    <#type_param as #crate_path::EncodeConstBinaryFormat>::decode_append_binary_format::<
                        #fill,
                        #alignment,
                        #width,
                        #zero_pad,
                        #alternate
                    >(
                        buffer,
                        &mut __zerolog_offset,
                        output,
                    );
                }
            }
        }),
        Some(b'o') if parsed.precision.is_none() => Some(if let Some(offset) = fixed_offset {
            quote! {
                unsafe {
                    <#type_param as #crate_path::EncodeConstOctalFormat>::decode_append_octal_format_at_offset::<
                        #fill,
                        #alignment,
                        #width,
                        #zero_pad,
                        #alternate
                    >(
                        buffer,
                        #offset,
                        output,
                    );
                }
            }
        } else {
            quote! {
                unsafe {
                    <#type_param as #crate_path::EncodeConstOctalFormat>::decode_append_octal_format::<
                        #fill,
                        #alignment,
                        #width,
                        #zero_pad,
                        #alternate
                    >(
                        buffer,
                        &mut __zerolog_offset,
                        output,
                    );
                }
            }
        }),
        Some(b'e' | b'E') if !parsed.alternate => {
            let ty = Literal::u8_unsuffixed(parsed.ty.unwrap());
            Some(if let Some(offset) = fixed_offset {
                quote! {
                    unsafe {
                        <#type_param as #crate_path::EncodeConstFloatFormat>::decode_append_float_format_at_offset::<
                            #fill,
                            #alignment,
                            #width,
                            #precision,
                            #zero_pad,
                            #ty
                        >(
                            buffer,
                            #offset,
                            output,
                        );
                    }
                }
            } else {
                quote! {
                    unsafe {
                        <#type_param as #crate_path::EncodeConstFloatFormat>::decode_append_float_format::<
                            #fill,
                            #alignment,
                            #width,
                            #precision,
                            #zero_pad,
                            #ty
                        >(
                            buffer,
                            &mut __zerolog_offset,
                            output,
                        );
                    }
                }
            })
        }
        _ => None,
    }
}

fn parse_const_format_spec(format_spec: &str) -> Option<ConstFormatSpec> {
    if format_spec.is_empty() {
        return None;
    }

    let spec = format_spec.strip_prefix(':').unwrap_or(format_spec);
    let bytes = spec.as_bytes();
    let mut index = 0usize;
    let mut parsed = ConstFormatSpec {
        fill: b' ',
        alignment: 0,
        width: 0,
        precision: None,
        alternate: false,
        zero_pad: false,
        ty: None,
    };

    if bytes.len() >= 2
        && let Some(alignment) = parse_const_default_alignment(bytes[1])
    {
        parsed.fill = bytes[0];
        parsed.alignment = alignment;
        index = 2;
    }

    if parsed.alignment == 0
        && let Some(alignment) = bytes.get(index).copied().and_then(parse_const_default_alignment)
    {
        parsed.alignment = alignment;
        index += 1;
    }

    if matches!(bytes.get(index), Some(b'#')) {
        parsed.alternate = true;
        index += 1;
    }

    if matches!(bytes.get(index), Some(b'0')) {
        parsed.zero_pad = true;
        parsed.fill = b'0';
        index += 1;
    }

    let width_start = index;
    while matches!(bytes.get(index), Some(value) if value.is_ascii_digit()) {
        index += 1;
    }
    if index > width_start {
        parsed.width = parse_usize_ascii(&bytes[width_start..index])?;
    }

    if matches!(bytes.get(index), Some(b'.')) {
        index += 1;
        let precision_start = index;
        while matches!(bytes.get(index), Some(value) if value.is_ascii_digit()) {
            index += 1;
        }
        parsed.precision = Some(if index > precision_start {
            parse_usize_ascii(&bytes[precision_start..index])?
        } else {
            0
        });
    }

    parsed.ty = bytes.get(index).copied();
    if parsed.ty.is_some() {
        index += 1;
    }

    (index == bytes.len()).then_some(parsed)
}

fn parse_const_default_alignment(value: u8) -> Option<u8> {
    match value {
        b'<' => Some(1),
        b'>' => Some(2),
        b'^' => Some(3),
        _ => None,
    }
}

fn parse_usize_ascii(bytes: &[u8]) -> Option<usize> {
    bytes.iter().try_fold(0usize, |value, digit| {
        value.checked_mul(10)?.checked_add((digit - b'0') as usize)
    })
}
