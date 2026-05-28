use proc_macro2::{Literal, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::{LitStr, Result};

use crate::{
    HeaderPatternInput, LogInput,
    format_string::{build_decoder_factory, parse_format_string},
};

/// Expands a `log!()` invocation, automatically selecting the fast path
/// (`EncodeFast`) when all format specifiers are bare `{}`, or the normal
/// path (`Encode`) when any specifier is present (e.g. `{:x}`, `{:.2}`).
///
/// This is the primary entry point — users never need to choose between
/// `log!` and `log_fast!`.
pub(crate) fn expand_log_auto(input: LogInput) -> TokenStream2 {
    let fragments = match parse_format_string(&input.format_string) {
        Ok(f) => f,
        Err(e) => return e.to_compile_error(),
    };

    let all_specs_empty = fragments.specs.iter().all(|s| s.is_empty());
    let (encode_trait, fast_path) = if all_specs_empty {
        (quote!(::ferrilog::EncodeFast), true)
    } else {
        (quote!(::ferrilog::Encode), false)
    };

    expand_log_inner(input, encode_trait, fast_path, fragments)
}

/// Core expansion logic, called by both `expand_log_auto` and legacy `expand_log`.
fn expand_log_inner(
    input: LogInput,
    encode_trait: TokenStream2,
    fast_path: bool,
    fragments: crate::format_string::ParsedFormat,
) -> TokenStream2 {
    if fragments.placeholder_count != input.args.len() {
        return syn::Error::new(
            input.format_string.span(),
            format!(
                "format placeholder count ({}) does not match argument count ({})",
                fragments.placeholder_count,
                input.args.len()
            ),
        )
        .to_compile_error();
    }

    if fast_path && fragments.specs.iter().any(|spec| !spec.is_empty()) {
        return syn::Error::new(
            input.format_string.span(),
            "log_fast! currently only supports bare `{}` placeholders (no format specifiers)",
        )
        .to_compile_error();
    }

    let level = input.level;
    let format_string = input.format_string;
    let arg_bindings: Vec<_> = input
        .args
        .iter()
        .enumerate()
        .map(|(index, expression)| {
            let ident = format_ident!("__ferrilog_arg_{index}");
            quote!(let #ident = &(#expression);)
        })
        .collect();
    let arg_idents: Vec<_> =
        (0..input.args.len()).map(|index| format_ident!("__ferrilog_arg_{index}")).collect();
    let type_params: Vec<_> =
        (0..input.args.len()).map(|index| format_ident!("A{index}")).collect();
    let generic_bounds: Vec<_> =
        type_params.iter().map(|type_param| quote!(#type_param: #encode_trait)).collect();
    let helper_args: Vec<_> = type_params
        .iter()
        .zip(arg_idents.iter())
        .map(|(type_param, arg_ident)| quote!(#arg_ident: &#type_param))
        .collect();

    let make_decoder =
        build_decoder_factory(&fragments, &arg_idents, &type_params, &encode_trait, fast_path);
    let encoded_size_fn = if fast_path {
        quote! {
            #[inline(always)]
            fn __ferrilog_encoded_size<A: ::ferrilog::EncodeFast>(value: &A) -> usize {
                if <A as ::ferrilog::Encode>::ENCODED_SIZE_HINT != ::ferrilog::ENCODED_SIZE_DYNAMIC {
                    <A as ::ferrilog::Encode>::ENCODED_SIZE_HINT
                } else {
                    <A as ::ferrilog::EncodeFast>::encoded_size_fast(value)
                }
            }

            #[inline(always)]
            unsafe fn __ferrilog_encode_one<A: ::ferrilog::EncodeFast>(
                value: &A,
                buffer: *mut u8,
                offset: &mut usize,
            ) {
                unsafe {
                    <A as ::ferrilog::EncodeFast>::encode_fast(value, buffer, offset);
                }
            }
        }
    } else {
        quote! {
            #[inline(always)]
            fn __ferrilog_encoded_size<A: ::ferrilog::Encode>(value: &A) -> usize {
                if <A as ::ferrilog::Encode>::ENCODED_SIZE_HINT != ::ferrilog::ENCODED_SIZE_DYNAMIC {
                    <A as ::ferrilog::Encode>::ENCODED_SIZE_HINT
                } else {
                    <A as ::ferrilog::Encode>::encoded_size(value)
                }
            }

            #[inline(always)]
            unsafe fn __ferrilog_encode_one<A: ::ferrilog::Encode>(
                value: &A,
                buffer: *mut u8,
                offset: &mut usize,
            ) {
                unsafe {
                    <A as ::ferrilog::Encode>::encode(value, buffer, offset);
                }
            }
        }
    };

    let decoder_invocation = if arg_idents.is_empty() {
        quote!(__ferrilog_make_decoder())
    } else {
        quote!(__ferrilog_make_decoder(#(#arg_idents),*))
    };

    let encoders: Vec<_> = arg_idents
        .iter()
        .map(|arg_ident| {
            quote! {
                unsafe {
                    __ferrilog_encode_one(#arg_ident, __ferrilog_buffer, &mut __ferrilog_offset);
                }
            }
        })
        .collect();
    let fixed_checks: Vec<_> = type_params
        .iter()
        .map(|type_param| {
            quote!(<#type_param as ::ferrilog::Encode>::ENCODED_SIZE_HINT != ::ferrilog::ENCODED_SIZE_DYNAMIC)
        })
        .collect();
    let fixed_base_terms: Vec<_> = type_params
        .iter()
        .map(|type_param| {
            quote! {
                if <#type_param as ::ferrilog::Encode>::ENCODED_SIZE_HINT != ::ferrilog::ENCODED_SIZE_DYNAMIC {
                    <#type_param as ::ferrilog::Encode>::ENCODED_SIZE_HINT
                } else {
                    0usize
                }
            }
        })
        .collect();
    let size_sums =
        arg_idents.iter().map(|arg_ident| quote!(+ __ferrilog_encoded_size(#arg_ident)));
    let fixed_offset_encoders: Vec<_> = type_params
        .iter()
        .zip(arg_idents.iter())
        .enumerate()
        .map(|(index, (type_param, arg_ident))| {
            let offset_terms: Vec<_> = fixed_checks
                .iter()
                .take(index)
                .zip(type_params.iter().take(index))
                .map(|(fixed_check, previous_type)| {
                    quote! {
                        .wrapping_add(if #fixed_check {
                            <#previous_type as ::ferrilog::Encode>::ENCODED_SIZE_HINT
                        } else {
                            0usize
                        })
                    }
                })
                .collect();
            if fast_path {
                quote! {
                    unsafe {
                        <#type_param as ::ferrilog::EncodeFast>::encode_fast_at_offset(
                            #arg_ident,
                            __ferrilog_buffer,
                            ::ferrilog::PAYLOAD_ARGS_OFFSET #(#offset_terms)*,
                        );
                    }
                }
            } else {
                quote! {
                    unsafe {
                        <#type_param as ::ferrilog::Encode>::encode_at_offset(
                            #arg_ident,
                            __ferrilog_buffer,
                            ::ferrilog::PAYLOAD_ARGS_OFFSET #(#offset_terms)*,
                        );
                    }
                }
            }
        })
        .collect();

    let payload_helpers = if arg_idents.is_empty() {
        quote! {
            let __ferrilog_payload_size: u32 = ::ferrilog::PAYLOAD_HEADER_SIZE as u32;
        }
    } else {
        quote! {
            #[inline(always)]
            fn __ferrilog_payload_size<#(#generic_bounds),*>(#(#helper_args),*) -> u32 {
                struct __FlashlogPayloadLayout<#(#type_params),*>(::std::marker::PhantomData<(#(#type_params),*)>);

                impl<#(#generic_bounds),*> __FlashlogPayloadLayout<#(#type_params),*> {
                    const ALL_FIXED: bool = true #(&& #fixed_checks)*;
                    const FIXED_PAYLOAD_SIZE: u32 =
                        (::ferrilog::PAYLOAD_HEADER_SIZE #(.wrapping_add(#fixed_base_terms))*) as u32;
                }

                if __FlashlogPayloadLayout::<#(#type_params),*>::ALL_FIXED {
                    __FlashlogPayloadLayout::<#(#type_params),*>::FIXED_PAYLOAD_SIZE
                } else {
                    (::ferrilog::PAYLOAD_HEADER_SIZE #(#size_sums)*) as u32
                }
            }

            #[inline(always)]
            unsafe fn __ferrilog_encode_payload<#(#generic_bounds),*>(
                #(#helper_args,)*
                __ferrilog_buffer: *mut u8,
            ) {
                struct __FlashlogPayloadLayout<#(#type_params),*>(::std::marker::PhantomData<(#(#type_params),*)>);

                impl<#(#generic_bounds),*> __FlashlogPayloadLayout<#(#type_params),*> {
                    const ALL_FIXED: bool = true #(&& #fixed_checks)*;
                }

                if __FlashlogPayloadLayout::<#(#type_params),*>::ALL_FIXED {
                    #(#fixed_offset_encoders)*
                } else {
                    let mut __ferrilog_offset = ::ferrilog::PAYLOAD_ARGS_OFFSET;
                    #(#encoders)*
                }
            }

            let __ferrilog_payload_size = __ferrilog_payload_size(#(#arg_idents),*);
        }
    };
    let encode_payload = if arg_idents.is_empty() {
        quote! {}
    } else {
        quote! {
            unsafe {
                __ferrilog_encode_payload(#(#arg_idents,)* __ferrilog_buffer);
            }
        }
    };

    quote! {
        {
            let __ferrilog_level: ::ferrilog::Level = #level;
            if ::ferrilog::check_level(__ferrilog_level) {
                let __ferrilog_span_id = ::ferrilog::current_span_id_u64();
                #(#arg_bindings)*
                #make_decoder
                #encoded_size_fn
                #payload_helpers

                // One-time initialization per call site: the OnceLock is
                // initialized on the first log call via get_or_init, which
                // is a single atomic load on the fast path after init.
                static SITE_INFO: std::sync::OnceLock<::ferrilog::StaticInfo> =
                    std::sync::OnceLock::new();
                let __ferrilog_info = SITE_INFO.get_or_init(|| {
                    ::ferrilog::StaticInfo {
                        level: __ferrilog_level,
                        location: concat!(file!(), ":", line!()),
                        format_string: #format_string,
                        decode: #decoder_invocation,
                    }
                });

                if let Some(__ferrilog_header) = ::ferrilog::alloc_message(__ferrilog_payload_size) {
                    unsafe {
                        let __ferrilog_buffer = (*__ferrilog_header).payload_pointer();
                        // Write info pointer
                        __ferrilog_buffer
                            .add(::ferrilog::PAYLOAD_INFO_OFFSET)
                            .cast::<*const ::ferrilog::StaticInfo>()
                            .write_unaligned(__ferrilog_info as *const ::ferrilog::StaticInfo);
                        // Write timestamp
                        let __ferrilog_timestamp = ::ferrilog::TimestampCounter::read_timestamp_counter();
                        __ferrilog_buffer
                            .add(::ferrilog::PAYLOAD_TIMESTAMP_OFFSET)
                            .cast::<i64>()
                            .write_unaligned(__ferrilog_timestamp);
                        // Write current span id
                        __ferrilog_buffer
                            .add(::ferrilog::PAYLOAD_SPAN_ID_OFFSET)
                            .cast::<u64>()
                            .write_unaligned(__ferrilog_span_id);
                        // Write encoded arguments
                        #encode_payload
                        (*__ferrilog_header).push(__ferrilog_payload_size);
                    }
                }
            }
        }
    }
}

/// Expands a `prewarm!` invocation.
///
/// `prewarm!` simply pre-allocates the calling thread's buffer so the first
/// real `log!` call avoids that allocation overhead.
pub(crate) fn expand_prewarm(input: LogInput) -> TokenStream2 {
    let fragments = match parse_format_string(&input.format_string) {
        Ok(fragments) => fragments,
        Err(error) => return error.to_compile_error(),
    };

    if fragments.placeholder_count != input.args.len() {
        return syn::Error::new(
            input.format_string.span(),
            format!(
                "format placeholder count ({}) does not match argument count ({})",
                fragments.placeholder_count,
                input.args.len()
            ),
        )
        .to_compile_error();
    }

    // prewarm! now only pre-allocates the thread buffer
    quote! {
        {
            ::ferrilog::thread_buffer::preallocate();
        }
    }
}

pub(crate) fn expand_set_header_pattern_static(input: HeaderPatternInput) -> TokenStream2 {
    let pattern = match parse_header_pattern_literal(&input.pattern) {
        Ok(pattern) => pattern,
        Err(error) => return error.to_compile_error(),
    };

    let reserve_base = Literal::usize_unsuffixed(pattern.fixed_len);
    let reserve_thread = pattern.uses_thread.then(|| quote!(+ thread_name.len())).into_iter();
    let reserve_location = pattern.uses_location.then(|| quote!(+ location.len())).into_iter();
    let reserve_span = pattern.uses_span.then(|| quote!(+ 20usize)).into_iter();
    let statements = pattern.segments.iter().map(|segment| match segment {
        HeaderSegment::Literal(bytes) => {
            let literal = Literal::byte_string(bytes);
            quote!(output.extend_from_slice(#literal);)
        }
        HeaderSegment::Date => quote! {
            date_cache.append_date(output, timestamp_nanoseconds);
        },
        HeaderSegment::Time => quote! {
            ::ferrilog::append_header_time(output, timestamp_nanoseconds);
        },
        HeaderSegment::Nanosecond => quote! {
            ::ferrilog::append_header_nanoseconds(output, timestamp_nanoseconds);
        },
        HeaderSegment::Level => quote! {
            output.extend_from_slice(level.as_bytes());
        },
        HeaderSegment::Span => quote! {
            ::ferrilog::append_span_id(output, span_id);
        },
        HeaderSegment::ThreadName => quote! {
            output.extend_from_slice(thread_name);
        },
        HeaderSegment::Location => quote! {
            output.extend_from_slice(location.as_bytes());
        },
    });

    quote! {
        {
            #[inline]
            fn __ferrilog_static_header_writer(
                date_cache: &mut ::ferrilog::HeaderDateCache,
                output: &mut ::std::vec::Vec<u8>,
                timestamp_nanoseconds: i64,
                level: ::ferrilog::Level,
                thread_name: &[u8],
                location: &str,
                span_id: u64,
            ) {
                output.reserve(#reserve_base #(#reserve_thread)* #(#reserve_location)* #(#reserve_span)*);
                #(#statements)*
            }

            ::ferrilog::set_static_header(__ferrilog_static_header_writer)
        }
    }
}

#[derive(Debug)]
enum HeaderSegment {
    Literal(Vec<u8>),
    Date,
    Time,
    Nanosecond,
    Level,
    Span,
    ThreadName,
    Location,
}

#[derive(Debug)]
struct ParsedHeaderPattern {
    segments: Vec<HeaderSegment>,
    fixed_len: usize,
    uses_thread: bool,
    uses_location: bool,
    uses_span: bool,
}

fn parse_header_pattern_literal(pattern: &LitStr) -> Result<ParsedHeaderPattern> {
    let source = pattern.value();
    let bytes = source.as_bytes();
    let mut index = 0usize;
    let mut literal = Vec::new();
    let mut segments = Vec::new();
    let mut fixed_len = 0usize;
    let mut uses_thread = false;
    let mut uses_location = false;
    let mut uses_span = false;

    while index < bytes.len() {
        match bytes[index] {
            b'{' => {
                if index + 1 < bytes.len() && bytes[index + 1] == b'{' {
                    literal.push(b'{');
                    index += 2;
                    continue;
                }

                if !literal.is_empty() {
                    fixed_len += literal.len();
                    segments.push(HeaderSegment::Literal(std::mem::take(&mut literal)));
                }

                let mut end = index + 1;
                while end < bytes.len() && bytes[end] != b'}' {
                    end += 1;
                }
                if end >= bytes.len() {
                    return Err(syn::Error::new(pattern.span(), "unclosed `{` in header pattern"));
                }

                let token = &source[index + 1..end];
                let segment = match token {
                    "date" => {
                        fixed_len += 10;
                        HeaderSegment::Date
                    }
                    "time" => {
                        fixed_len += 8;
                        HeaderSegment::Time
                    }
                    "ns" => {
                        fixed_len += 9;
                        HeaderSegment::Nanosecond
                    }
                    "level" => {
                        fixed_len += 3;
                        HeaderSegment::Level
                    }
                    "span" => {
                        uses_span = true;
                        HeaderSegment::Span
                    }
                    "thread" => {
                        uses_thread = true;
                        HeaderSegment::ThreadName
                    }
                    "location" => {
                        uses_location = true;
                        HeaderSegment::Location
                    }
                    _ => {
                        return Err(syn::Error::new(
                            pattern.span(),
                            format!("unknown header token: {{{token}}}"),
                        ));
                    }
                };
                segments.push(segment);
                index = end + 1;
            }
            b'}' => {
                if index + 1 < bytes.len() && bytes[index + 1] == b'}' {
                    literal.push(b'}');
                    index += 2;
                } else {
                    return Err(syn::Error::new(pattern.span(), "stray `}` in header pattern"));
                }
            }
            byte => {
                literal.push(byte);
                index += 1;
            }
        }
    }

    if !literal.is_empty() {
        fixed_len += literal.len();
        segments.push(HeaderSegment::Literal(literal));
    }

    Ok(ParsedHeaderPattern { segments, fixed_len, uses_thread, uses_location, uses_span })
}
