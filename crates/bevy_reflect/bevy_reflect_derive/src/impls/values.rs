use crate::fq_std::{FQBox, FQClone, FQOption, FQResult};
use crate::impls::impl_typed;
use crate::utility::WhereClauseOptions;
use crate::ReflectMeta;
use proc_macro::TokenStream;
use quote::quote;

use super::impl_full_reflect;

/// Implements `GetTypeRegistration` and `Reflect` for the given type data.
pub(crate) fn impl_value(meta: &ReflectMeta) -> TokenStream {
    let bevy_reflect_path = meta.bevy_reflect_path();
    let type_name = meta.type_name();

    let hash_fn = meta.traits().get_hash_impl(bevy_reflect_path);
    let partial_eq_fn = meta.traits().get_partial_eq_impl(bevy_reflect_path);
    let debug_fn = meta.traits().get_debug_impl();

    #[cfg(feature = "documentation")]
    let with_docs = {
        let doc = quote::ToTokens::to_token_stream(meta.doc());
        Some(quote!(.with_docs(#doc)))
    };
    #[cfg(not(feature = "documentation"))]
    let with_docs: Option<proc_macro2::TokenStream> = None;

    let where_clause_options = WhereClauseOptions::default();
    let typed_impl = impl_typed(
        type_name,
        meta.generics(),
        &where_clause_options,
        quote! {
            let info = #bevy_reflect_path::ValueInfo::new::<Self>() #with_docs;
            #bevy_reflect_path::TypeInfo::Value(info)
        },
        bevy_reflect_path,
    );
    
    let impl_full_reflect = impl_full_reflect(meta);

    let (impl_generics, ty_generics, where_clause) = meta.generics().split_for_impl();
    
    let get_type_registration_impl = meta.get_type_registration(&where_clause_options);

    TokenStream::from(quote! {
        #impl_full_reflect

        #get_type_registration_impl

        #typed_impl

        impl #impl_generics #bevy_reflect_path::PartialReflect for #type_name #ty_generics #where_clause  {
            #[inline]
            fn type_name(&self) -> &str {
                ::core::any::type_name::<Self>()
            }

            #[inline]
            fn get_type_info(&self) -> &'static #bevy_reflect_path::TypeInfo {
                <Self as #bevy_reflect_path::Typed>::type_info()
            }

            fn as_full(&self) -> #FQOption<&dyn #bevy_reflect_path::Reflect> {
                Some(self)
            }

            fn as_full_mut(&mut self) -> #FQOption<&mut dyn #bevy_reflect_path::Reflect> {
                Some(self)
            }

            fn into_full(self: Box<Self>) -> #FQResult<Box<dyn #bevy_reflect_path::Reflect>, Box<dyn #bevy_reflect_path::PartialReflect>> {
                Ok(self)
            }

            fn as_partial(&self) -> &dyn #bevy_reflect_path::PartialReflect {
                self
            }

            fn as_partial_mut(&mut self) -> &mut dyn #bevy_reflect_path::PartialReflect {
                self
            }

            fn into_partial(self: #FQBox<Self>) -> #FQBox<dyn #bevy_reflect_path::PartialReflect> {
                self
            }

            #[inline]
            fn clone_value(&self) -> #FQBox<dyn #bevy_reflect_path::PartialReflect> {
                #FQBox::new(#FQClone::clone(self))
            }

            #[inline]
            fn apply(&mut self, value: &dyn #bevy_reflect_path::PartialReflect) {
                if let #FQOption::Some(value) = <dyn #bevy_reflect_path::PartialReflect>::try_downcast_ref::<Self>(value) {
                    *self = #FQClone::clone(value);
                } else {
                    panic!("Value is not {}.", ::core::any::type_name::<Self>());
                }
            }

            fn reflect_ref(&self) -> #bevy_reflect_path::ReflectRef {
                #bevy_reflect_path::ReflectRef::Value(self)
            }

            fn reflect_mut(&mut self) -> #bevy_reflect_path::ReflectMut {
                #bevy_reflect_path::ReflectMut::Value(self)
            }

            fn reflect_owned(self: #FQBox<Self>) -> #bevy_reflect_path::ReflectOwned {
                #bevy_reflect_path::ReflectOwned::Value(self)
            }

            #hash_fn

            #partial_eq_fn

            #debug_fn
        }
    })
}
