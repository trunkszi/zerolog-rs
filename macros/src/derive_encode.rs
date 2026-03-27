use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::DeriveInput;

/// Expands the `#[derive(Encode)]` macro, implementing the `Encode` trait
/// for a `Copy + 'static + Display` type using raw memory copy for encoding
/// and `Display` formatting for decoding.
pub(crate) fn expand_derive_encode(input: DeriveInput) -> TokenStream2 {
    let name = input.ident;
    let generics = input.generics;
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let assertion_name =
        format_ident!("__ferrilog_assert_encode_{}", name, span = Span::call_site());

    quote! {
        const _: () = {
            fn #assertion_name<T>()
            where
                T: Copy + 'static + ::std::fmt::Display,
            {}

            let _ = #assertion_name::<#name #ty_generics>;
        };

        impl #impl_generics ::ferrilog::Encode for #name #ty_generics #where_clause {
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
                    ::ferrilog::append_display_const_default::<
                        Self,
                        b' ',
                        { ::ferrilog::FORMAT_ALIGN_NONE },
                        0,
                        { ::ferrilog::FORMAT_PRECISION_NONE },
                        false,
                    >(output, value);
                } else {
                    ::ferrilog::append_display_with_spec(output, value, format_spec);
                }
            }
        }

        unsafe impl #impl_generics ::ferrilog::EncodeFast for #name #ty_generics #where_clause {
            #[inline]
            unsafe fn decode_and_append_fast(
                buffer: *const u8,
                offset: &mut usize,
                output: &mut Vec<u8>,
            ) {
                let value = unsafe { buffer.add(*offset).cast::<Self>().read_unaligned() };
                *offset += core::mem::size_of::<Self>();
                ::ferrilog::append_display_const_default::<
                    Self,
                    b' ',
                    { ::ferrilog::FORMAT_ALIGN_NONE },
                    0,
                    { ::ferrilog::FORMAT_PRECISION_NONE },
                    false,
                >(output, value);
            }
        }
    }
}
