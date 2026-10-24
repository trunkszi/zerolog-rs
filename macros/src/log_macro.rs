use proc_macro2::{Literal, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::{LitStr, Result};

use crate::{
    HeaderPatternInput, LogInput,
    format_string::{build_decoder_factory, parse_format_string},
};

/// Generates type-specific site metadata and bounded payload encoding.
pub(crate) fn expand_log_auto(mut input: LogInput) -> TokenStream2 {
    let fragments = match parse_format_string(&input.format_string) {
        Ok(value) => value,
        Err(error) => return error.to_compile_error(),
    };
    let positions = match resolve_arguments(&mut input, &fragments) {
        Ok(value) => value,
        Err(error) => return error.to_compile_error(),
    };
    let crate_path = &input.crate_path;
    let fast_path = fragments.specs.iter().all(|s| s.is_empty());
    let encode_trait =
        if fast_path { quote!(#crate_path::EncodeFast) } else { quote!(#crate_path::Encode) };
    let bound_ids: Vec<_> = (0..input.args.len())
        .map(|i| format_ident!("__zerolog_arg_{i}", span = proc_macro2::Span::mixed_site()))
        .collect();
    let bindings = input.args.iter().zip(&bound_ids).map(|(expr, id)| quote!(let #id = &(#expr);));
    let ids: Vec<_> = positions.iter().map(|&i| bound_ids[i].clone()).collect();
    let types: Vec<_> = (0..ids.len()).map(|i| format_ident!("A{i}")).collect();
    let bounds: Vec<_> = types.iter().map(|t| quote!(#t: #encode_trait)).collect();
    let helper_ids: Vec<_> = (0..ids.len()).map(|i| format_ident!("__zerolog_value_{i}")).collect();
    let helper_args: Vec<_> =
        types.iter().zip(&helper_ids).map(|(t, id)| quote!(#id: &#t)).collect();
    let generics = if types.is_empty() { quote!() } else { quote!(<#(#bounds),*>) };
    let arguments = if types.is_empty() { quote!() } else { quote!(<#(#types),*>) };
    let decoder = build_decoder_factory(&fragments, &types, &encode_trait, fast_path, crate_path);
    let decoder_ref = if types.is_empty() {
        quote!(__zerolog_decode)
    } else {
        quote!(__zerolog_decode::<#(#types),*>)
    };
    let format = input.format_string;
    let infos = ["TRC", "DBG", "INF", "WRN", "ERR", "OFF"].iter().map(|name| {
        let level = format_ident!("{name}");
        quote!(#crate_path::StaticInfo {
            level: #crate_path::Level::#level,
            location: concat!(file!(), ":", line!()),
            format_string: #format,
            decode: #decoder_ref,
        })
    });
    let site = if types.is_empty() {
        quote!(
            struct __ZerologSite;
        )
    } else {
        quote!(struct __ZerologSite<#(#types),*>(::core::marker::PhantomData<(#(#types,)*)>);)
    };
    let n = types.len();
    let fixed = types.iter().map(|t| quote!(<#t as #crate_path::Encode>::ENCODED_SIZE_HINT != #crate_path::ENCODED_SIZE_DYNAMIC));
    let offset_steps = types.iter().enumerate().map(|(i, t)| {
        quote! {
            offsets[#i] = cursor;
            cursor = cursor.saturating_add(<#t as #crate_path::Encode>::ENCODED_SIZE_HINT);
        }
    });
    let size_steps = helper_ids.iter().map(|id| {
        quote! {
            total = total.saturating_add(__zerolog_size(#id));
        }
    });
    let size_call = if fast_path {
        quote!(#crate_path::EncodeFast::encoded_size_fast(value))
    } else {
        quote!(#crate_path::Encode::encoded_size(value))
    };
    let fixed_encoders = types.iter().zip(&helper_ids).enumerate().map(|(i, (t, id))| {
        let method = if fast_path {
            format_ident!("encode_fast_at_offset")
        } else {
            format_ident!("encode_at_offset")
        };
        quote!(unsafe { <#t as #encode_trait>::#method(#id, buffer, offsets[#i]); })
    });
    let sequential_encoders = helper_ids.iter().map(|id| {
        let method = if fast_path { format_ident!("encode_fast") } else { format_ident!("encode") };
        quote!(unsafe { #encode_trait::#method(#id, buffer, &mut offset); })
    });
    let layout = if types.is_empty() {
        quote!()
    } else {
        quote! {
            struct __ZerologPayloadLayout<#(#types),*>(::core::marker::PhantomData<(#(#types,)*)>);
            impl #generics __ZerologPayloadLayout #arguments {
                const ALL_FIXED: bool = true #(&& #fixed)*;
                const OFFSETS: [usize; #n] = {
                    let mut offsets = [0; #n];
                    let mut cursor = #crate_path::PAYLOAD_ARGS_OFFSET;
                    #(#offset_steps)*
                    let _ = cursor;
                    offsets
                };
            }
            #[inline(always)]
            fn __zerolog_size<A: #encode_trait>(value: &A) -> usize {
                if <A as #crate_path::Encode>::ENCODED_SIZE_HINT != #crate_path::ENCODED_SIZE_DYNAMIC {
                    <A as #crate_path::Encode>::ENCODED_SIZE_HINT
                } else { #size_call }
            }
            #[inline(always)]
            fn __zerolog_payload_size #generics (#(#helper_args),*) -> u32 {
                let mut total = #crate_path::PAYLOAD_HEADER_SIZE;
                #(#size_steps)*
                u32::try_from(total).unwrap_or(u32::MAX)
            }
            #[inline(always)]
            unsafe fn __zerolog_encode_payload #generics (#(#helper_args,)* buffer: *mut u8) {
                if __ZerologPayloadLayout::<#(#types),*>::ALL_FIXED {
                    let offsets = __ZerologPayloadLayout::<#(#types),*>::OFFSETS;
                    #(#fixed_encoders)*
                } else {
                    let mut offset = #crate_path::PAYLOAD_ARGS_OFFSET;
                    #(#sequential_encoders)*
                }
            }
        }
    };
    let payload_size = if types.is_empty() {
        quote!(#crate_path::PAYLOAD_HEADER_SIZE as u32)
    } else {
        quote!(__zerolog_payload_size(#(#ids),*))
    };
    let encode = if types.is_empty() {
        quote!()
    } else {
        quote!(unsafe { __zerolog_encode_payload(#(#ids,)* buffer); })
    };
    let site_infos = if types.is_empty() {
        quote!(__ZerologSite::INFOS)
    } else {
        quote!(__ZerologSite::<#(#types),*>::INFOS)
    };
    let level = input.level;
    let runtime_level = format_ident!("__zerolog_level", span = proc_macro2::Span::mixed_site());
    let span_id = format_ident!("__zerolog_span_id", span = proc_macro2::Span::mixed_site());
    quote! {{
        let #runtime_level: #crate_path::Level = #level;
        if #crate_path::check_level(#runtime_level) {
            let #span_id = #crate_path::current_span_id_u64();
            #(#bindings)*
            {
            #decoder
            #site
            impl #generics __ZerologSite #arguments {
                const INFOS: [#crate_path::StaticInfo; 6] = [#(#infos),*];
            }
            #[inline(always)]
            fn __zerolog_site_info #generics (#(#helper_args,)* level: #crate_path::Level) -> &'static #crate_path::StaticInfo {
                &#site_infos[level as usize]
            }
            #layout
            let info = __zerolog_site_info(#(#ids,)* #runtime_level);
            let payload_size = #payload_size;
            if payload_size != u32::MAX && let Some(header) = #crate_path::alloc_message(payload_size) {
                unsafe {
                    let buffer = #crate_path::spsc_queue::MessageHeader::payload_pointer_raw(header);
                    buffer.add(#crate_path::PAYLOAD_INFO_OFFSET).cast::<*const #crate_path::StaticInfo>().write_unaligned(info);
                    buffer.add(#crate_path::PAYLOAD_TIMESTAMP_OFFSET).cast::<i64>().write_unaligned(#crate_path::TimestampCounter::read_timestamp_counter());
                    buffer.add(#crate_path::PAYLOAD_SPAN_ID_OFFSET).cast::<u64>().write_unaligned(#span_id);
                    #encode
                    (*header).push(payload_size);
                }
            }
            }
        }
    }}
}

/// Resolve references once; the payload follows placeholder order for sequential decoding.
fn resolve_arguments(
    input: &mut LogInput,
    format: &crate::format_string::ParsedFormat,
) -> Result<Vec<usize>> {
    use crate::format_string::FormatArgument;
    let supplied = input.args.len();
    let mut used = vec![false; supplied];
    let mut captures = std::collections::BTreeMap::new();
    let mut positions = Vec::with_capacity(format.placeholder_count);
    for arg in &format.arguments {
        let index = match arg {
            FormatArgument::Position(index) => {
                if *index >= supplied {
                    return Err(syn::Error::new(
                        input.format_string.span(),
                        "format argument index out of range",
                    ));
                }
                used[*index] = true;
                *index
            }
            FormatArgument::Capture(name) => {
                *captures.entry(name.to_string()).or_insert_with(|| {
                    let index = input.args.len();
                    input.args.push(syn::parse_quote!(#name));
                    index
                })
            }
        };
        positions.push(index);
    }
    if used.iter().any(|used| !used) {
        return Err(syn::Error::new(input.format_string.span(), "unused format argument"));
    }
    Ok(positions)
}

/// Preallocates the calling thread's producer buffer.
pub(crate) fn expand_prewarm(mut input: LogInput) -> TokenStream2 {
    let format = match parse_format_string(&input.format_string) {
        Ok(value) => value,
        Err(error) => return error.to_compile_error(),
    };
    if let Err(error) = resolve_arguments(&mut input, &format) {
        return error.to_compile_error();
    }
    let path = input.crate_path;
    quote!({ #path::thread_buffer::preallocate(); })
}

pub(crate) fn expand_set_header_pattern_static(input: HeaderPatternInput) -> TokenStream2 {
    let crate_path = &input.crate_path;
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
            #crate_path::append_header_time(output, timestamp_nanoseconds);
        },
        HeaderSegment::Nanosecond => quote! {
            #crate_path::append_header_nanoseconds(output, timestamp_nanoseconds);
        },
        HeaderSegment::Level => quote! {
            output.extend_from_slice(level.as_bytes());
        },
        HeaderSegment::Span => quote! {
            #crate_path::append_span_id(output, span_id);
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
            fn __zerolog_static_header_writer(
                date_cache: &mut #crate_path::HeaderDateCache,
                output: &mut ::std::vec::Vec<u8>,
                timestamp_nanoseconds: i64,
                level: #crate_path::Level,
                thread_name: &[u8],
                location: &str,
                span_id: u64,
            ) {
                output.reserve(#reserve_base #(#reserve_thread)* #(#reserve_location)* #(#reserve_span)*);
                #(#statements)*
            }

            #crate_path::set_static_header(__zerolog_static_header_writer)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn token_count(tokens: TokenStream2) -> usize {
        tokens
            .into_iter()
            .map(|token| match token {
                proc_macro2::TokenTree::Group(group) => 1 + token_count(group.stream()),
                _ => 1,
            })
            .sum()
    }

    /// Doubling fixed-width arguments does not rebuild all preceding offsets per field.
    #[test]
    fn fixed_layout_expansion_is_linear() {
        let size = |n: usize| {
            let format = vec!["{}"; n].join(" ");
            let args = vec!["1u8"; n].join(",");
            let input =
                syn::parse_str::<LogInput>(&format!("::zerolog::Level::INF, {format:?}, {args}"))
                    .expect("valid fixed-width invocation");
            token_count(expand_log_auto(input))
        };
        assert!(size(256) < 3 * size(128));
    }
}
