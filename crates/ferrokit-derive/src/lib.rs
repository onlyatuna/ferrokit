// SPDX-License-Identifier: LGPL-3.0-only
#![allow(linker_messages)]

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{parse_macro_input, Attribute, DeriveInput, Fields, Ident, LitStr, Type};

mod invoke;
mod slots;

use invoke::{argument_bindings, invoker_fn, type_name};

/// Derives `QObject` and generates a static `MetaObject` reflection descriptor (`QMetaObject`).
///
/// Automatically creates:
/// - `fn object_data(&self) -> &ObjectData`
/// - `fn object_data_mut(&mut self) -> &mut ObjectData`
/// - `fn as_qobject_any(&self) -> Option<&dyn Any>`
/// - `fn as_qobject_any_mut(&mut self) -> Option<&mut dyn Any>`
/// - `fn meta_object(&self) -> &'static MetaObject`
///
/// Supports field-level `#[property]` and `#[signal]` attributes:
/// `#[property(notify = "signal")]` emits the named `#[signal]` field when a write changes the value.
/// ```ignore
/// #[derive(QObject)]
/// #[qobject(class_name = "MyButton")]
/// struct MyButton {
///     data: ObjectData,
///     #[property]
///     text: String,
///     #[property(readonly)]
///     count: i32,
///     #[signal]
///     clicked: Signal<()>,
/// }
/// ```
/// Signal fields are invokable by name; invoking one emits it, as `emit` does. A derived signal
/// gets blockSignals suppression and `sender()` only when built with `Signal::with_owner(&data)`;
/// `Signal::new()` leaves the signal unowned.
#[proc_macro_derive(QObject, attributes(qobject, property, signal))]
pub fn derive_qobject(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    if !input.generics.params.is_empty() {
        return syn::Error::new_spanned(
            &input.generics,
            "`#[derive(QObject)]` does not support generic structs",
        )
        .to_compile_error()
        .into();
    }
    if !matches!(
        &input.data,
        syn::Data::Struct(data) if matches!(&data.fields, Fields::Named(_))
    ) {
        return syn::Error::new_spanned(
            &input,
            "`#[derive(QObject)]` requires a struct with named fields",
        )
        .to_compile_error()
        .into();
    }
    let name = &input.ident;
    let name_str = name.to_string();

    // Check for class_name override in #[qobject(class_name = "...")]
    let mut class_name = name_str.clone();
    for attr in &input.attrs {
        if attr.path().is_ident("qobject") {
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("class_name") {
                    let s: LitStr = meta.value()?.parse()?;
                    class_name = s.value();
                }
                Ok(())
            });
        }
    }

    // Inspect fields for ObjectData and #[property]
    let mut data_field_ident: Option<Ident> = None;
    let mut properties = Vec::new();

    let mut signals = Vec::new();

    if let syn::Data::Struct(data_struct) = &input.data {
        if let Fields::Named(fields_named) = &data_struct.fields {
            for field in &fields_named.named {
                let f_ident = field.ident.clone().unwrap();
                let f_type = &field.ty;

                // Identify the ObjectData field (named `data` or with type ending in `ObjectData`)
                if f_ident == "data" || is_object_data_type(f_type) {
                    data_field_ident = Some(f_ident.clone());
                }

                // Check for #[property] attribute
                for attr in &field.attrs {
                    if attr.path().is_ident("property") {
                        let (is_readonly, notify) = match parse_property_attr(attr) {
                            Ok(parsed) => parsed,
                            Err(err) => return err.to_compile_error().into(),
                        };
                        let prop_name = f_ident.to_string();
                        properties.push((
                            f_ident.clone(),
                            f_type.clone(),
                            prop_name,
                            is_readonly,
                            notify,
                        ));
                    }
                }

                for attr in &field.attrs {
                    if attr.path().is_ident("signal") {
                        if let Some(payload) = signal_payload_type(f_type) {
                            signals.push((f_ident.clone(), payload));
                        } else {
                            return syn::Error::new_spanned(
                                f_type,
                                "`#[signal]` fields must have type `Signal<T>`",
                            )
                            .to_compile_error()
                            .into();
                        }
                    }
                }
            }
        }
    }
    for (_, f_type, _, is_readonly, notify) in &properties {
        let Some(notify_lit) = notify else {
            continue;
        };
        let notify_name = notify_lit.value();
        if *is_readonly {
            return syn::Error::new(
                notify_lit.span(),
                "`notify` requires a writable property; remove `readonly`",
            )
            .to_compile_error()
            .into();
        }
        let Some((_, payload_types)) = signals.iter().find(|(sig, _)| *sig == notify_name) else {
            return syn::Error::new(
                notify_lit.span(),
                format!(
                    "`notify = \"{notify_name}\"` must name a `#[signal]` field of this struct"
                ),
            )
            .to_compile_error()
            .into();
        };
        let elements = payload_elements(payload_types);
        let payload_matches = matches!(
            elements.as_slice(),
            [only] if quote!(#only).to_string() == quote!(#f_type).to_string()
        );
        if !payload_matches {
            return syn::Error::new(
                notify_lit.span(),
                format!(
                    "`notify = \"{notify_name}\"` requires `Signal<{}>` matching the property type",
                    quote!(#f_type)
                ),
            )
            .to_compile_error()
            .into();
        }
    }
    if data_field_ident.is_none() {
        return syn::Error::new_spanned(
            name,
            "`#[derive(QObject)]` requires a field named `data` or a field of type `ObjectData`",
        )
        .to_compile_error()
        .into();
    }

    let (data_access, data_access_mut) = if let Some(field) = &data_field_ident {
        (quote! { &self.#field }, quote! { &mut self.#field })
    } else {
        (quote! { &self.data }, quote! { &mut self.data })
    };

    // Generate MetaProperty items
    let mut meta_properties = Vec::new();
    let meta_obj_ident = Ident::new(&format!("__{}_META_OBJECT", name), name.span());

    let data_ident = data_field_ident
        .clone()
        .unwrap_or_else(|| Ident::new("data", name.span()));
    for (f_ident, f_type, _prop_name, is_readonly, notify) in &properties {
        let is_writable = !*is_readonly;
        let getter_fn_ident = Ident::new(&format!("__ferrokit_get_{}_{}", name, f_ident), name.span());
        let setter_fn_ident = Ident::new(&format!("__ferrokit_set_{}_{}", name, f_ident), name.span());
        let notify_ident = notify
            .as_ref()
            .map(|lit| Ident::new(&lit.value(), lit.span()));

        let (getter_impl, setter_impl) = generate_property_accessors(
            name,
            f_ident,
            f_type,
            &getter_fn_ident,
            &setter_fn_ident,
            is_writable,
            notify_ident.as_ref(),
            &data_ident,
        );

        meta_properties.push(quote! {
            #getter_impl
            #setter_impl
        });
    }

    let prop_descriptors: Vec<_> = properties
        .iter()
        .map(|(f_ident, f_type, prop_name, is_readonly, notify)| {
            let is_writable = !*is_readonly;
            let getter_fn_ident =
                Ident::new(&format!("__ferrokit_get_{}_{}", name, f_ident), name.span());
            let setter_fn_ident =
                Ident::new(&format!("__ferrokit_set_{}_{}", name, f_ident), name.span());

            let setter_ref = if is_writable {
                quote! { Some(#setter_fn_ident) }
            } else {
                quote! { None }
            };

            let type_name_str = quote!(#f_type).to_string();
            let notify_option = match notify {
                Some(lit) => quote! { Some(#lit) },
                None => quote! { None },
            };

            quote! {
                ::ferrokit_core::meta::MetaProperty::new(
                    #prop_name,
                    #type_name_str,
                    true,
                    #is_writable,
                    false,
                    false,
                    #notify_option,
                    Some(#getter_fn_ident),
                    #setter_ref,
                )
            }
        })
        .collect();

    let props_ident = format_ident!("{}_PROPERTIES", meta_obj_ident);

    let mut signal_descriptors = Vec::new();
    let mut signal_invokers = Vec::new();
    for (field, payload) in &signals {
        let elems = payload_elements(payload);
        let parameter_types: Vec<String> = elems.iter().map(type_name).collect();
        let name_str = field.to_string();
        let signature = LitStr::new(
            &format!("{}({})", name_str, parameter_types.join(",")),
            field.span(),
        );
        let parameter_literals: Vec<LitStr> = parameter_types
            .iter()
            .map(|ty| LitStr::new(ty, field.span()))
            .collect();
        let invoker = format_ident!("__ferrokit_invoke_signal_{}_{}", name, field);
        signal_descriptors.push(quote! {
            ::ferrokit_core::meta::MetaMethod::new(
                #name_str,
                #signature,
                "()",
                &[#(#parameter_literals),*],
                &[],
                ::ferrokit_core::meta::MethodType::Signal,
                ::ferrokit_core::meta::Access::Public,
                Some(#invoker),
            )
        });

        let (arg_idents, bindings) = argument_bindings(&elems);
        let payload_value = if matches!(payload, Type::Tuple(_)) {
            quote! { (#(#arg_idents,)*) }
        } else {
            quote! { #(#arg_idents)* }
        };
        signal_invokers.push(invoker_fn(
            &invoker,
            &name,
            &bindings,
            quote! {
                let __ferrokit_payload: #payload = #payload_value;
                this.#field.emit(&__ferrokit_payload);
                Ok(::ferrokit_core::variant::Variant::Invalid)
            },
        ));
    }

    let methods_ident = format_ident!("{}_METHODS", meta_obj_ident);
    let expanded = quote! {
        #(#meta_properties)*
        #(#signal_invokers)*

        #[allow(non_upper_case_globals)]
        static #methods_ident: ::std::sync::LazyLock<::std::vec::Vec<::ferrokit_core::meta::MetaMethod>> =
            ::std::sync::LazyLock::new(|| {
                use ::ferrokit_core::meta::SlotMethodsFallback as _;
                let mut methods: ::std::vec::Vec<::ferrokit_core::meta::MetaMethod> =
                    ::std::vec![ #(#signal_descriptors),* ];
                methods.extend(#name::__ferrokit_slot_methods());
                methods
            });

        #[allow(non_upper_case_globals)]
        static #props_ident: ::std::sync::LazyLock<::std::vec::Vec<::ferrokit_core::meta::MetaProperty>> =
            ::std::sync::LazyLock::new(|| ::std::vec![ #(#prop_descriptors),* ]);

        #[allow(non_upper_case_globals)]
        static #meta_obj_ident: ::std::sync::LazyLock<::ferrokit_core::meta::MetaObject> =
            ::std::sync::LazyLock::new(|| {
                ::ferrokit_core::meta::MetaObject::new(
                    #class_name,
                    Some(&::ferrokit_core::meta::QOBJECT_META_OBJECT),
                    #methods_ident.as_slice(),
                    #props_ident.as_slice(),
                    &[],
                    &[],
                )
            });

        impl ::ferrokit_core::object::QObject for #name {
            fn object_data(&self) -> &::ferrokit_core::object::ObjectData {
                #data_access
            }

            fn object_data_mut(&mut self) -> &mut ::ferrokit_core::object::ObjectData {
                #data_access_mut
            }

            fn as_qobject_any(&self) -> Option<&dyn ::std::any::Any> {
                Some(self)
            }

            fn as_qobject_any_mut(&mut self) -> Option<&mut dyn ::std::any::Any> {
                Some(self)
            }

            fn meta_object(&self) -> &'static ::ferrokit_core::meta::MetaObject {
                &*#meta_obj_ident
            }
        }
    };

    TokenStream::from(expanded)
}

/// Registers the `#[slot]` methods of an inherent impl in the type's meta object, so they can be
/// invoked by name or signature through `MetaObject::invoke_method`.
///
/// ```ignore
/// #[slots]
/// impl Panel {
///     #[slot]
///     fn set_caption(&mut self, text: String) { /* ... */ }
/// }
/// ```
/// Parameters must be owned types; a return value must convert into `Variant`. Put all `#[slot]`
/// methods of one type in a single `#[slots]` block.
#[proc_macro_attribute]
pub fn slots(_attr: TokenStream, item: TokenStream) -> TokenStream {
    slots::expand(item.into()).into()
}

/// Marks a method of a `#[slots]` impl block. Outside such a block it is a compile error.
#[proc_macro_attribute]
pub fn slot(_attr: TokenStream, item: TokenStream) -> TokenStream {
    slots::expand_standalone(item.into()).into()
}

/// Returns the payload `T` of a `Signal<T>` field type; `Signal<(A, B)>` yields the tuple type.
fn signal_payload_type(ty: &Type) -> Option<Type> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    if segment.ident != "Signal" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    let mut generic_args = args.args.iter();
    let Some(syn::GenericArgument::Type(payload)) = generic_args.next() else {
        return None;
    };
    if generic_args.next().is_some() {
        return None;
    }
    Some(payload.clone())
}

/// Parameter types of a signal payload: a tuple's elements, or the payload type itself.
fn payload_elements(payload: &Type) -> Vec<Type> {
    match payload {
        Type::Tuple(tuple) => tuple.elems.iter().cloned().collect(),
        other => vec![other.clone()],
    }
}

fn is_object_data_type(ty: &Type) -> bool {
    if let Type::Path(type_path) = ty {
        if let Some(segment) = type_path.path.segments.last() {
            return segment.ident == "ObjectData";
        }
    }
    false
}

fn parse_property_attr(attr: &Attribute) -> syn::Result<(bool, Option<LitStr>)> {
    let mut readonly = false;
    let mut notify = None;
    // A bare `#[property]` has no nested options to parse.
    if matches!(attr.meta, syn::Meta::Path(_)) {
        return Ok((readonly, notify));
    }
    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("readonly") {
            readonly = true;
            Ok(())
        } else if meta.path.is_ident("notify") {
            notify = Some(meta.value()?.parse::<LitStr>()?);
            Ok(())
        } else {
            Err(meta
                .error("unknown `#[property]` option; expected `readonly` or `notify = \"...\"`"))
        }
    })?;
    Ok((readonly, notify))
}
#[allow(clippy::too_many_arguments)]
fn generate_property_accessors(
    struct_name: &Ident,
    field_ident: &Ident,
    field_type: &Type,
    getter_ident: &Ident,
    setter_ident: &Ident,
    is_writable: bool,
    notify_ident: Option<&Ident>,
    data_ident: &Ident,
) -> (proc_macro2::TokenStream, proc_macro2::TokenStream) {
    let getter = quote! {
        #[allow(non_snake_case)]
        fn #getter_ident(obj: &dyn ::std::any::Any) -> ::ferrokit_core::variant::Variant {
            if let Some(s) = obj.downcast_ref::<#struct_name>() {
                ::ferrokit_core::variant::Variant::from(s.#field_ident.clone())
            } else {
                ::ferrokit_core::variant::Variant::Invalid
            }
        }
    };

    let (changed_check, notify_emit) = match notify_ident {
        Some(notify_ident) => (
            quote! { let changed = s.#field_ident != converted; },
            quote! {
                if changed && !s.#data_ident.signals_blocked() {
                    let new_value = s.#field_ident.clone();
                    s.#notify_ident.emit(&new_value);
                }
            },
        ),
        None => (quote! {}, quote! {}),
    };

    let setter = if is_writable {
        quote! {
            #[allow(non_snake_case)]
            fn #setter_ident(
                obj: &mut dyn ::std::any::Any,
                val: ::ferrokit_core::variant::Variant,
            ) -> Result<(), ::ferrokit_core::meta::InvokeError> {
                if let Some(s) = obj.downcast_mut::<#struct_name>() {
                    // Try to convert variant to property type
                    if let Some(converted) = ::ferrokit_core::variant::convert_variant_to_type::<#field_type>(&val) {
                        #changed_check
                        s.#field_ident = converted;
                        #notify_emit
                        Ok(())
                    } else {
                        Err(::ferrokit_core::meta::InvokeError::TypeMismatch {
                            index: 0,
                            expected: stringify!(#field_type),
                        })
                    }
                } else {
                    Err(::ferrokit_core::meta::InvokeError::TargetBorrowFailed)
                }
            }
        }
    } else {
        quote! {
            #[allow(non_snake_case)]
            fn #setter_ident(
                _obj: &mut dyn ::std::any::Any,
                _val: ::ferrokit_core::variant::Variant,
            ) -> Result<(), ::ferrokit_core::meta::InvokeError> {
                Err(::ferrokit_core::meta::InvokeError::ExecutionFailed("Property is read-only".into()))
            }
        }
    };

    (getter, setter)
}
