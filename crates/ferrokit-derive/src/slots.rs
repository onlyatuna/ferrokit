// SPDX-License-Identifier: LGPL-3.0-only
//! `#[slots]`: registers the `#[slot]` methods of an inherent impl in the type's meta object.

use crate::invoke::{argument_bindings, invoker_fn, type_name};
use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::{
    Attribute, Error, FnArg, Ident, ImplItem, ImplItemFn, ItemImpl, LitStr, ReturnType, Type,
    Visibility,
};

/// Expands `#[slots] impl Type { ... }`.
pub(crate) fn expand(item: TokenStream) -> TokenStream {
    match expand_impl(item) {
        Ok(tokens) => tokens,
        Err(error) => error.to_compile_error(),
    }
}

/// Expands a `#[slot]` that is not inside a `#[slots]` impl: an error, keeping the item.
pub(crate) fn expand_standalone(item: TokenStream) -> TokenStream {
    let error = Error::new(
        Span::call_site(),
        "`#[slot]` is only valid on a method of an impl block marked `#[slots]`",
    )
    .to_compile_error();
    quote! {
        #error
        #item
    }
}

fn expand_impl(item: TokenStream) -> syn::Result<TokenStream> {
    let mut imp: ItemImpl = syn::parse2(item)?;
    if let Some((_, trait_path, _)) = &imp.trait_ {
        return Err(Error::new_spanned(
            trait_path,
            "`#[slots]` applies to inherent impls only",
        ));
    }
    if !imp.generics.params.is_empty() {
        return Err(Error::new_spanned(
            &imp.generics,
            "`#[slots]` does not support generic impls",
        ));
    }
    let self_ident = self_type_ident(&imp.self_ty)?;
    let self_ty = &*imp.self_ty;

    let mut descriptors = Vec::new();
    let mut invokers = Vec::new();
    for item in &mut imp.items {
        let ImplItem::Fn(method) = item else {
            continue;
        };
        if !take_slot_marker(&mut method.attrs) {
            continue;
        }
        let (descriptor, invoker) = slot_method(method, &self_ident, self_ty)?;
        descriptors.push(descriptor);
        invokers.push(invoker);
    }

    Ok(quote! {
        #imp
        #(#invokers)*
        impl #self_ty {
            #[doc(hidden)]
            pub(crate) fn __ferrokit_slot_methods() -> ::std::vec::Vec<::ferrokit_core::meta::MetaMethod> {
                ::std::vec![ #(#descriptors),* ]
            }
        }
    })
}

/// The bare struct name of `self_ty`, used to name the generated invokers.
fn self_type_ident(self_ty: &Type) -> syn::Result<Ident> {
    if let Type::Path(type_path) = self_ty
        && type_path.qself.is_none()
        && type_path.path.segments.len() == 1
    {
        let segment = &type_path.path.segments[0];
        if segment.arguments.is_empty() {
            return Ok(segment.ident.clone());
        }
    }
    Err(Error::new_spanned(
        self_ty,
        "`#[slots]` needs a plain type name without generic arguments",
    ))
}

/// Removes the `#[slot]` marker from `attrs`; returns whether it was present.
fn take_slot_marker(attrs: &mut Vec<Attribute>) -> bool {
    let before = attrs.len();
    attrs.retain(|attr| !attr.path().is_ident("slot"));
    attrs.len() != before
}

/// Descriptor expression and module-level invoker for one `#[slot]` method.
fn slot_method(
    method: &ImplItemFn,
    self_ident: &Ident,
    self_ty: &Type,
) -> syn::Result<(TokenStream, TokenStream)> {
    let sig = &method.sig;
    if !sig.generics.params.is_empty() {
        return Err(Error::new_spanned(
            &sig.generics,
            "`#[slot]` methods cannot be generic",
        ));
    }
    if sig.asyncness.is_some() || sig.constness.is_some() {
        return Err(Error::new_spanned(
            &sig.ident,
            "`#[slot]` methods must be plain `fn`s",
        ));
    }
    match sig.inputs.first() {
        Some(FnArg::Receiver(receiver)) if receiver.reference.is_some() => {}
        _ => {
            return Err(Error::new_spanned(
                &sig.ident,
                "`#[slot]` methods must take `&self` or `&mut self`",
            ));
        }
    }

    let mut param_types = Vec::new();
    for arg in sig.inputs.iter().skip(1) {
        let FnArg::Typed(pat_type) = arg else {
            return Err(Error::new_spanned(arg, "unexpected receiver"));
        };
        if matches!(&*pat_type.ty, Type::Reference(_)) {
            return Err(Error::new_spanned(
                &pat_type.ty,
                "`#[slot]` parameters must be owned types such as `String` or `i64`; references are not supported",
            ));
        }
        param_types.push((*pat_type.ty).clone());
    }

    let (return_name, returns_unit) = match &sig.output {
        ReturnType::Default => ("()".to_string(), true),
        ReturnType::Type(_, ty) if matches!(&**ty, Type::Tuple(t) if t.elems.is_empty()) => {
            ("()".to_string(), true)
        }
        ReturnType::Type(_, ty) => (type_name(ty), false),
    };

    let method_ident = &sig.ident;
    let name_str = method_ident.to_string();
    let parameter_names: Vec<String> = param_types.iter().map(type_name).collect();
    let signature = format!("{}({})", name_str, parameter_names.join(","));
    let parameter_literals: Vec<LitStr> = parameter_names
        .iter()
        .map(|ty| LitStr::new(ty, method_ident.span()))
        .collect();
    let access = if matches!(method.vis, Visibility::Public(_)) {
        quote! { ::ferrokit_core::meta::Access::Public }
    } else {
        quote! { ::ferrokit_core::meta::Access::Private }
    };

    let invoker = format_ident!("__ferrokit_invoke_slot_{}_{}", self_ident, method_ident);
    let descriptor = quote! {
        ::ferrokit_core::meta::MetaMethod::new(
            #name_str,
            #signature,
            #return_name,
            &[#(#parameter_literals),*],
            &[],
            ::ferrokit_core::meta::MethodType::Slot,
            #access,
            Some(#invoker),
        )
    };

    let (arg_idents, bindings) = argument_bindings(&param_types);
    let call = quote! { this.#method_ident(#(#arg_idents),*) };
    let body = if returns_unit {
        quote! {
            #call;
            Ok(::ferrokit_core::variant::Variant::Invalid)
        }
    } else {
        quote! {
            Ok(::std::convert::Into::<::ferrokit_core::variant::Variant>::into(#call))
        }
    };
    let invoker_tokens = invoker_fn(&invoker, &self_ty, &bindings, body);
    Ok((descriptor, invoker_tokens))
}
