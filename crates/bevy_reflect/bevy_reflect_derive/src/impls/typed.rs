use crate::utility::{extend_where_clause, WhereClauseOptions};
use quote::{quote, ToTokens};
use syn::Generics;
use syn::{spanned::Spanned, LitStr};

use crate::{
    derive_data::{ReflectMeta, ReflectTypePath},
    utility::wrap_in_option,
};

fn combine_generics(
    ty_generics: Vec<proc_macro2::TokenStream>,
    generics: &Generics,
) -> impl Iterator<Item = proc_macro2::TokenStream> {
    let const_generic_strings: Vec<_> = generics
        .const_params()
        .map(|param| {
            let ident = &param.ident;
            let ty = &param.ty;

            quote! {
                &<#ty as ::std::string::ToString>::to_string(&#ident)
            }
        })
        .collect();

    let mut generics = ty_generics
        .into_iter()
        .chain(const_generic_strings.into_iter())
        .flat_map(|t| [", ".to_token_stream(), t]);
    generics.next(); // Skip first comma.
    generics
}

/// Returns an expression for a `&'static str`,
/// representing either a [long path] or [short path].
///
/// [long path]: ReflectTypePath::non_generic_type_path
/// [short path]: ReflectTypePath::non_generic_short_path
fn type_path_generator(long_path: bool, meta: &ReflectMeta) -> proc_macro2::TokenStream {
    let type_path = meta.type_path();
    let generics = type_path.generics();
    let bevy_reflect_path = meta.bevy_reflect_path();

    if let ReflectTypePath::Primitive(name) = type_path {
        let name = LitStr::new(&name.to_string(), name.span());
        return quote!(#name);
    }

    let ty_generic_paths: Vec<_> = generics
        .type_params()
        .map(|param| {
            let ident = &param.ident;
            quote! {
                <#ident as #bevy_reflect_path::TypePath>
            }
        })
        .collect();

    let (path, ty_generics) = if long_path {
        let ty_generics: Vec<_> = ty_generic_paths
            .iter()
            .map(|cell| {
                quote! {
                    #cell::type_path()
                }
            })
            .collect();

        (type_path.non_generic_type_path(), ty_generics)
    } else {
        let ty_generics: Vec<_> = ty_generic_paths
            .iter()
            .map(|cell| {
                quote! {
                    #cell::short_type_path()
                }
            })
            .collect();

        (type_path.non_generic_short_path(), ty_generics)
    };

    let generics = combine_generics(ty_generics, generics);

    quote! {
        ::std::borrow::ToOwned::to_owned(::core::concat!(#path, "<"))
            #(+ #generics)*
            + ">"
    }
}

/// Returns an expression for a `NonGenericTypeCell` or `GenericTypeCell`  to contain `'static` references.
fn static_typed_cell(
    meta: &ReflectMeta,
    property: TypedProperty,
    generator: proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    let bevy_reflect_path = meta.bevy_reflect_path();
    if meta.impl_is_generic() {
        let cell_type = match property {
            TypedProperty::TypePath => quote!(GenericTypePathCell),
            TypedProperty::TypeInfo => quote!(GenericTypeInfoCell),
        };

        quote! {
            static CELL: #bevy_reflect_path::utility::#cell_type = #bevy_reflect_path::utility::#cell_type::new();
            CELL.get_or_insert::<Self, _>(|| {
                #generator
            })
        }
    } else {
        let cell_type = match property {
            TypedProperty::TypePath => unreachable!(
                "cannot have a non-generic type path cell. use string literals instead."
            ),
            TypedProperty::TypeInfo => quote!(NonGenericTypeInfoCell),
        };

        quote! {
            static CELL: #bevy_reflect_path::utility::#cell_type = #bevy_reflect_path::utility::#cell_type::new();
            CELL.get_or_set(|| {
                #generator
            })
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum TypedProperty {
    TypeInfo,
    TypePath,
}

pub(crate) fn impl_type_path(
    meta: &ReflectMeta,
    where_clause_options: &WhereClauseOptions,
) -> proc_macro2::TokenStream {
    let type_path = meta.type_path();
    let bevy_reflect_path = meta.bevy_reflect_path();

    let (long_type_path, short_type_path) = if meta.impl_is_generic() {
        let long_path_cell = static_typed_cell(
            meta,
            TypedProperty::TypePath,
            type_path_generator(true, meta),
        );
        let short_path_cell = static_typed_cell(
            meta,
            TypedProperty::TypePath,
            type_path_generator(false, meta),
        );
        (
            long_path_cell.to_token_stream(),
            short_path_cell.to_token_stream(),
        )
    } else {
        (
            type_path.non_generic_type_path(),
            type_path.non_generic_short_path(),
        )
    };

    let type_ident = wrap_in_option(type_path.type_ident());
    let module_path = wrap_in_option(type_path.module_path());
    let crate_name = wrap_in_option(type_path.crate_name());

    let primitive_assert = if let ReflectTypePath::Primitive(_) = type_path {
        Some(quote! {
            const _: () = {
                mod private_scope {
                    // Compiles if it can be named when there are no imports.
                    type AssertIsPrimitive = #type_path;
                }
            };
        })
    } else {
        None
    };

    let (impl_generics, ty_generics, where_clause) = type_path.generics().split_for_impl();

    // Add Typed bound for each active field
    let where_reflect_clause = extend_where_clause(where_clause, where_clause_options);

    quote! {
        #primitive_assert

        impl #impl_generics #bevy_reflect_path::TypePath for #type_path #ty_generics #where_reflect_clause {
            fn type_path() -> &'static str {
                #long_type_path
            }

            fn short_type_path() -> &'static str {
                #short_type_path
            }

            fn type_ident() -> Option<&'static str> {
                #type_ident
            }

            fn crate_name() -> Option<&'static str> {
                #crate_name
            }

            fn module_path() -> Option<&'static str> {
                #module_path
            }
        }
    }
}

pub(crate) fn impl_typed(
    meta: &ReflectMeta,
    where_clause_options: &WhereClauseOptions,
    type_info_generator: proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    let type_path = meta.type_path();
    let bevy_reflect_path = meta.bevy_reflect_path();

    let type_info_cell = static_typed_cell(meta, TypedProperty::TypeInfo, type_info_generator);

    let (impl_generics, ty_generics, where_clause) = type_path.generics().split_for_impl();

    let where_reflect_clause = extend_where_clause(where_clause, where_clause_options);

    quote! {
        impl #impl_generics #bevy_reflect_path::Typed for #type_path #ty_generics #where_reflect_clause {
            fn type_info() -> &'static #bevy_reflect_path::TypeInfo {
                #type_info_cell
            }
        }
    }
}
