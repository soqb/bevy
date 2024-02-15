//! Contains code related to container attributes for reflected types.
//!
//! A container attribute is an attribute which applies to an entire struct or enum
//! as opposed to a particular field or variant. An example of such an attribute is
//! the derive helper attribute for `Reflect`, which looks like:
//! `#[reflect(PartialEq, Default, ...)]` and `#[reflect_value(PartialEq, Default, ...)]`.

use crate::derive_data::{
    ReflectImplSource, ReflectProvenance, ReflectTraitToImpl, ReflectTypeKind,
};
use crate::utility;
use crate::utility::terminated_parser;
use bevy_macro_utils::fq_std::{FQAny, FQOption};
use proc_macro2::{Ident, Span};
use quote::quote_spanned;
use syn::ext::IdentExt;
use syn::parse::ParseStream;
use syn::spanned::Spanned;
use syn::{
    parenthesized, token, Expr, ExprPath, LitBool, MetaList, MetaNameValue, Path, Token,
    WhereClause,
};

mod kw {
    syn::custom_keyword!(from_reflect);
    syn::custom_keyword!(type_path);
    syn::custom_keyword!(Debug);
    syn::custom_keyword!(PartialEq);
    syn::custom_keyword!(Hash);
    syn::custom_keyword!(no_field_bounds);
    syn::custom_keyword!(container_default);
}

// The "special" trait idents that are used internally for reflection.
// Received via attributes like `#[reflect(PartialEq, Hash, ...)]`
const DEBUG_ATTR: &str = "Debug";
const PARTIAL_EQ_ATTR: &str = "PartialEq";
const HASH_ATTR: &str = "Hash";

// Attributes for `FromReflect` implementation
const FROM_REFLECT_ATTR: &str = "from_reflect";

// Attributes for `TypePath` implementation
const TYPE_PATH_ATTR: &str = "type_path";

/// Used to specify a default container value for `FromReflect` impls for non-local types.
const CONTAINER_DEFAULT_ATTR: &str = "container_default";

// The error message to show when a trait/type is specified multiple times
const CONFLICTING_TYPE_DATA_MESSAGE: &str = "conflicting type data registration";

/// A marker for trait implementations registered via the `Reflect` derive macro.
#[derive(Clone, Default)]
pub(crate) enum TraitImpl {
    /// The trait is not registered as implemented.
    #[default]
    NotImplemented,

    /// The trait is registered as implemented.
    Implemented(Span),

    /// The trait is registered with a custom function rather than an actual implementation.
    Custom(Path, Span),
}

impl TraitImpl {
    /// Merges this [`TraitImpl`] with another.
    ///
    /// Update `self` with whichever value is not [`TraitImpl::NotImplemented`].
    /// If `other` is [`TraitImpl::NotImplemented`], then `self` is not modified.
    /// An error is returned if neither value is [`TraitImpl::NotImplemented`].
    pub fn merge(&mut self, other: TraitImpl) -> Result<(), syn::Error> {
        match (&self, other) {
            (TraitImpl::NotImplemented, value) => {
                *self = value;
                Ok(())
            }
            (_, TraitImpl::NotImplemented) => Ok(()),
            (_, TraitImpl::Implemented(span) | TraitImpl::Custom(_, span)) => {
                Err(syn::Error::new(span, CONFLICTING_TYPE_DATA_MESSAGE))
            }
        }
    }
}

/// A collection of attributes used for deriving `FromReflect`.
#[derive(Clone, Default)]
pub(crate) struct FromReflectAttrs {
    auto_derive: Option<LitBool>,
    /// Exclusively for creating default values in [`impl_reflect`]'s `FromReflect` impl.
    ///
    /// [`impl_reflect`]: macro@crate::impl_reflect
    pub container_default: Option<ExprPath>,
}

impl FromReflectAttrs {
    /// Returns true if `FromReflect` should be automatically derived as part of the `Reflect` derive.
    pub fn should_auto_derive(&self) -> bool {
        self.auto_derive
            .as_ref()
            .map(|lit| lit.value())
            .unwrap_or(true)
    }

    /// Merges this [`FromReflectAttrs`] with another.
    pub fn merge(&mut self, other: FromReflectAttrs) -> Result<(), syn::Error> {
        if let Some(new) = other.auto_derive {
            if let Some(existing) = &self.auto_derive {
                if existing.value() != new.value() {
                    return Err(syn::Error::new(
                        new.span(),
                        format!("`{FROM_REFLECT_ATTR}` already set to {}", existing.value()),
                    ));
                }
            } else {
                self.auto_derive = Some(new);
            }
        }

        if let Some(container_default) = other.container_default {
            self.insert_container_default(container_default)?;
        }

        Ok(())
    }

    pub fn insert_container_default(
        &mut self,
        container_default: ExprPath,
    ) -> Result<(), syn::Error> {
        match &mut self.container_default {
            Some(old) => {
                return Err(syn::Error::new(
                    old.span(),
                    format!("`{CONTAINER_DEFAULT_ATTR}` already set"),
                ))
            }
            old @ None => *old = Some(container_default),
        }

        Ok(())
    }
}

/// A collection of attributes used for deriving `TypePath` via the `Reflect` derive.
///
/// Note that this differs from the attributes used by the `TypePath` derive itself,
/// which look like `[type_path = "my_crate::foo"]`.
/// The attributes used by reflection take the form `#[reflect(type_path = false)]`.
///
/// These attributes should only be used for `TypePath` configuration specific to
/// deriving `Reflect`.
#[derive(Clone, Default)]
pub(crate) struct TypePathAttrs {
    auto_derive: Option<LitBool>,
}

impl TypePathAttrs {
    /// Returns true if `TypePath` should be automatically derived as part of the `Reflect` derive.
    pub fn should_auto_derive(&self) -> bool {
        self.auto_derive
            .as_ref()
            .map(|lit| lit.value())
            .unwrap_or(true)
    }

    /// Merges this [`TypePathAttrs`] with another.
    pub fn merge(&mut self, other: TypePathAttrs) -> Result<(), syn::Error> {
        if let Some(new) = other.auto_derive {
            if let Some(existing) = &self.auto_derive {
                if existing.value() != new.value() {
                    return Err(syn::Error::new(
                        new.span(),
                        format!("`{TYPE_PATH_ATTR}` already set to {}", existing.value()),
                    ));
                }
            } else {
                self.auto_derive = Some(new);
            }
        }

        Ok(())
    }
}

/// A collection of traits that have been registered for a reflected type.
///
/// This keeps track of a few traits that are utilized internally for reflection
/// (we'll call these traits _special traits_ within this context), but it
/// will also keep track of all registered traits. Traits are registered as part of the
/// `Reflect` derive macro using the helper attribute: `#[reflect(...)]`.
///
/// The list of special traits are as follows:
/// * `Debug`
/// * `Hash`
/// * `PartialEq`
///
/// When registering a trait, there are a few things to keep in mind:
/// * Traits must have a valid `Reflect{}` struct in scope. For example, `Default`
///   needs `bevy_reflect::prelude::ReflectDefault` in scope.
/// * Traits must be single path identifiers. This means you _must_ use `Default`
///   instead of `std::default::Default` (otherwise it will try to register `Reflectstd`!)
/// * A custom function may be supplied in place of an actual implementation
///   for the special traits (but still follows the same single-path identifier
///   rules as normal).
///
/// # Example
///
/// Registering the `Default` implementation:
///
/// ```ignore (bevy_reflect is not accessible from this crate)
/// // Import ReflectDefault so it's accessible by the derive macro
/// use bevy_reflect::prelude::ReflectDefault;
///
/// #[derive(Reflect, Default)]
/// #[reflect(Default)]
/// struct Foo;
/// ```
///
/// Registering the `Hash` implementation:
///
/// ```ignore (bevy_reflect is not accessible from this crate)
/// // `Hash` is a "special trait" and does not need (nor have) a ReflectHash struct
///
/// #[derive(Reflect, Hash)]
/// #[reflect(Hash)]
/// struct Foo;
/// ```
///
/// Registering the `Hash` implementation using a custom function:
///
/// ```ignore (bevy_reflect is not accessible from this crate)
/// // This function acts as our `Hash` implementation and
/// // corresponds to the `Reflect::reflect_hash` method.
/// fn get_hash(foo: &Foo) -> Option<u64> {
///   Some(123)
/// }
///
/// #[derive(Reflect)]
/// // Register the custom `Hash` function
/// #[reflect(Hash(get_hash))]
/// struct Foo;
/// ```
///
/// > __Note:__ Registering a custom function only works for special traits.
///
#[derive(Default, Clone)]
pub(crate) struct ContainerAttributes {
    debug: TraitImpl,
    hash: TraitImpl,
    partial_eq: TraitImpl,
    from_reflect_attrs: FromReflectAttrs,
    type_path_attrs: TypePathAttrs,
    custom_where: Option<WhereClause>,
    no_field_bounds: bool,
    idents: Vec<Ident>,
}

impl ContainerAttributes {
    /// Parse a comma-separated list of container attributes.
    ///
    /// # Example
    /// - `Hash, Debug(custom_debug), MyTrait`
    pub fn parse_terminated(
        input: ParseStream,
        provenance: ReflectProvenance,
    ) -> syn::Result<Self> {
        let mut this = Self::default();

        terminated_parser(Token![,], |stream| {
            this.parse_container_attribute(stream, provenance)
        })(input)?;

        Ok(this)
    }

    /// Parse the contents of a `#[reflect(...)]` attribute into a [`ContainerAttributes`] instance.
    ///
    /// # Example
    /// - `#[reflect(Hash, Debug(custom_debug), MyTrait)]`
    /// - `#[reflect(no_field_bounds)]`
    pub fn parse_meta_list(meta: &MetaList, provenance: ReflectProvenance) -> syn::Result<Self> {
        meta.parse_args_with(|stream: ParseStream| Self::parse_terminated(stream, provenance))
    }

    /// Parse a single container attribute.
    fn parse_container_attribute(
        &mut self,
        input: ParseStream,
        provenance: ReflectProvenance,
    ) -> syn::Result<()> {
        let lookahead = input.lookahead1();
        if lookahead.peek(Token![where]) {
            self.parse_custom_where(input)
        } else if lookahead.peek(kw::from_reflect) {
            self.parse_from_reflect(input, provenance.trait_)
        } else if lookahead.peek(kw::type_path) {
            self.parse_type_path(input, provenance.trait_)
        } else if lookahead.peek(kw::no_field_bounds) {
            self.parse_no_field_bounds(input)
        } else if lookahead.peek(kw::Debug) {
            self.parse_debug(input)
        } else if lookahead.peek(kw::PartialEq) {
            self.parse_partial_eq(input)
        } else if lookahead.peek(kw::Hash) {
            self.parse_hash(input)
        } else if lookahead.peek(Ident::peek_any) {
            self.parse_ident(input)
        } else if lookahead.peek(kw::container_default) {
            self.parse_container_default(input, provenance)
        } else {
            Err(lookahead.error())
        }
    }

    /// Parse an ident (for registration).
    ///
    /// Examples:
    /// - `#[reflect(MyTrait)]` (registers `ReflectMyTrait`)
    fn parse_ident(&mut self, input: ParseStream) -> syn::Result<()> {
        let ident = input.parse::<Ident>()?;

        if input.peek(token::Paren) {
            return Err(syn::Error::new(ident.span(), format!(
                "only [{DEBUG_ATTR:?}, {PARTIAL_EQ_ATTR:?}, {HASH_ATTR:?}] may specify custom functions",
            )));
        }

        let ident_name = ident.to_string();

        // Create the reflect ident
        let mut reflect_ident = utility::get_reflect_ident(&ident_name);
        // We set the span to the old ident so any compile errors point to that ident instead
        reflect_ident.set_span(ident.span());

        add_unique_ident(&mut self.idents, reflect_ident)?;

        Ok(())
    }

    /// Parse special `Debug` registration.
    ///
    /// Examples:
    /// - `#[reflect(Debug)]`
    /// - `#[reflect(Debug(custom_debug_fn))]`
    fn parse_debug(&mut self, input: ParseStream) -> syn::Result<()> {
        let ident = input.parse::<kw::Debug>()?;

        if input.peek(token::Paren) {
            let content;
            parenthesized!(content in input);
            let path = content.parse::<Path>()?;
            self.debug.merge(TraitImpl::Custom(path, ident.span))?;
        } else {
            self.debug = TraitImpl::Implemented(ident.span);
        }

        Ok(())
    }

    /// Parse special `PartialEq` registration.
    ///
    /// Examples:
    /// - `#[reflect(PartialEq)]`
    /// - `#[reflect(PartialEq(custom_partial_eq_fn))]`
    fn parse_partial_eq(&mut self, input: ParseStream) -> syn::Result<()> {
        let ident = input.parse::<kw::PartialEq>()?;

        if input.peek(token::Paren) {
            let content;
            parenthesized!(content in input);
            let path = content.parse::<Path>()?;
            self.partial_eq.merge(TraitImpl::Custom(path, ident.span))?;
        } else {
            self.partial_eq = TraitImpl::Implemented(ident.span);
        }

        Ok(())
    }

    /// Parse special `Hash` registration.
    ///
    /// Examples:
    /// - `#[reflect(Hash)]`
    /// - `#[reflect(Hash(custom_hash_fn))]`
    fn parse_hash(&mut self, input: ParseStream) -> syn::Result<()> {
        let ident = input.parse::<kw::Hash>()?;

        if input.peek(token::Paren) {
            let content;
            parenthesized!(content in input);
            let path = content.parse::<Path>()?;
            self.hash.merge(TraitImpl::Custom(path, ident.span))?;
        } else {
            self.hash = TraitImpl::Implemented(ident.span);
        }

        Ok(())
    }

    /// Parse `no_field_bounds` attribute.
    ///
    /// Examples:
    /// - `#[reflect(no_field_bounds)]`
    fn parse_no_field_bounds(&mut self, input: ParseStream) -> syn::Result<()> {
        input.parse::<kw::no_field_bounds>()?;
        self.no_field_bounds = true;
        Ok(())
    }

    /// Parse `where` attribute.
    ///
    /// Examples:
    /// - `#[reflect(where T: Debug)]`
    fn parse_custom_where(&mut self, input: ParseStream) -> syn::Result<()> {
        self.custom_where = Some(input.parse()?);
        Ok(())
    }

    /// Parse `from_reflect` attribute.
    ///
    /// Examples:
    /// - `#[reflect(from_reflect = false)]`
    fn parse_from_reflect(
        &mut self,
        input: ParseStream,
        trait_: ReflectTraitToImpl,
    ) -> syn::Result<()> {
        let pair = input.parse::<MetaNameValue>()?;
        let value = extract_bool(&pair.value, |lit| {
            // Override `lit` if this is a `FromReflect` derive.
            // This typically means a user is opting out of the default implementation
            // from the `Reflect` derive and using the `FromReflect` derive directly instead.
            (trait_ == ReflectTraitToImpl::FromReflect)
                .then(|| LitBool::new(true, Span::call_site()))
                .unwrap_or_else(|| lit.clone())
        })?;

        self.from_reflect_attrs.auto_derive = Some(value);

        Ok(())
    }

    /// Parse `type_path` attribute.
    ///
    /// Examples:
    /// - `#[reflect(type_path = false)]`
    fn parse_type_path(
        &mut self,
        input: ParseStream,
        trait_: ReflectTraitToImpl,
    ) -> syn::Result<()> {
        let pair = input.parse::<MetaNameValue>()?;
        let value = extract_bool(&pair.value, |lit| {
            // Override `lit` if this is a `FromReflect` derive.
            // This typically means a user is opting out of the default implementation
            // from the `Reflect` derive and using the `FromReflect` derive directly instead.
            (trait_ == ReflectTraitToImpl::TypePath)
                .then(|| LitBool::new(true, Span::call_site()))
                .unwrap_or_else(|| lit.clone())
        })?;

        self.type_path_attrs.auto_derive = Some(value);

        Ok(())
    }

    /// Parse `container_default` attribute.
    ///
    /// Examples:
    /// - `#[reflect(container_default)]`
    /// - `#[reflect(container_default = my_func)]`
    fn parse_container_default(
        &mut self,
        input: ParseStream,
        provenance: ReflectProvenance,
    ) -> syn::Result<()> {
        if provenance.source == ReflectImplSource::DeriveLocalType {
            return Err(syn::Error::new(
                input.span(),
                format!("`#[reflect({CONTAINER_DEFAULT_ATTR} = ...)]` is only applicable when using `impl_reflect`."),
            ));
        } else if !matches!(
            provenance.type_kind,
            ReflectTypeKind::Struct | ReflectTypeKind::TupleStruct
        ) {
            return Err(syn::Error::new(
                input.span(),
                format!(
                    "`#[reflect({CONTAINER_DEFAULT_ATTR} = ...)]` is only applicable on structs."
                ),
            ));
        }

        input.parse::<kw::container_default>()?;
        input.parse::<Token![=]>()?;

        let path = input.parse()?;
        self.from_reflect_attrs.insert_container_default(path)
    }

    /// The list of reflected traits by their reflected ident (i.e. `ReflectDefault` for `Default`).
    pub fn idents(&self) -> &[Ident] {
        &self.idents
    }

    /// The `FromReflect` configuration found within `#[reflect(...)]` attributes on this type.
    #[allow(clippy::wrong_self_convention)]
    pub fn from_reflect_attrs(&self) -> &FromReflectAttrs {
        &self.from_reflect_attrs
    }

    /// The `TypePath` configuration found within `#[reflect(...)]` attributes on this type.
    pub fn type_path_attrs(&self) -> &TypePathAttrs {
        &self.type_path_attrs
    }

    /// Returns the implementation of `Reflect::reflect_hash` as a `TokenStream`.
    ///
    /// If `Hash` was not registered, returns `None`.
    pub fn get_hash_impl(&self, bevy_reflect_path: &Path) -> Option<proc_macro2::TokenStream> {
        match &self.hash {
            &TraitImpl::Implemented(span) => Some(quote_spanned! {span=>
                fn reflect_hash(&self) -> #FQOption<u64> {
                    use ::core::hash::{Hash, Hasher};
                    let mut hasher = #bevy_reflect_path::utility::reflect_hasher();
                    Hash::hash(&#FQAny::type_id(self), &mut hasher);
                    Hash::hash(self, &mut hasher);
                    #FQOption::Some(Hasher::finish(&hasher))
                }
            }),
            &TraitImpl::Custom(ref impl_fn, span) => Some(quote_spanned! {span=>
                fn reflect_hash(&self) -> #FQOption<u64> {
                    #FQOption::Some(#impl_fn(self))
                }
            }),
            TraitImpl::NotImplemented => None,
        }
    }

    /// Returns the implementation of `Reflect::reflect_partial_eq` as a `TokenStream`.
    ///
    /// If `PartialEq` was not registered, returns `None`.
    pub fn get_partial_eq_impl(
        &self,
        bevy_reflect_path: &Path,
    ) -> Option<proc_macro2::TokenStream> {
        match &self.partial_eq {
            &TraitImpl::Implemented(span) => Some(quote_spanned! {span=>
                fn reflect_partial_eq(&self, value: &dyn #bevy_reflect_path::Reflect) -> #FQOption<bool> {
                    let value = <dyn #bevy_reflect_path::Reflect>::as_any(value);
                    if let #FQOption::Some(value) = <dyn #FQAny>::downcast_ref::<Self>(value) {
                        #FQOption::Some(::core::cmp::PartialEq::eq(self, value))
                    } else {
                        #FQOption::Some(false)
                    }
                }
            }),
            &TraitImpl::Custom(ref impl_fn, span) => Some(quote_spanned! {span=>
                fn reflect_partial_eq(&self, value: &dyn #bevy_reflect_path::Reflect) -> #FQOption<bool> {
                    #FQOption::Some(#impl_fn(self, value))
                }
            }),
            TraitImpl::NotImplemented => None,
        }
    }

    /// Returns the implementation of `Reflect::debug` as a `TokenStream`.
    ///
    /// If `Debug` was not registered, returns `None`.
    pub fn get_debug_impl(&self) -> Option<proc_macro2::TokenStream> {
        match &self.debug {
            &TraitImpl::Implemented(span) => Some(quote_spanned! {span=>
                fn debug(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                    ::core::fmt::Debug::fmt(self, f)
                }
            }),
            &TraitImpl::Custom(ref impl_fn, span) => Some(quote_spanned! {span=>
                fn debug(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                    #impl_fn(self, f)
                }
            }),
            TraitImpl::NotImplemented => None,
        }
    }

    /// The custom where configuration found within `#[reflect(...)]` attributes on this type.
    pub fn custom_where(&self) -> Option<&WhereClause> {
        self.custom_where.as_ref()
    }

    /// Returns true if the `no_field_bounds` attribute was found on this type.
    pub fn no_field_bounds(&self) -> bool {
        self.no_field_bounds
    }

    /// Merges the trait implementations of this [`ContainerAttributes`] with another one.
    ///
    /// An error is returned if the two [`ContainerAttributes`] have conflicting implementations.
    pub fn merge(&mut self, other: ContainerAttributes) -> Result<(), syn::Error> {
        // Destructuring is used to help ensure that all fields are merged
        let Self {
            debug,
            hash,
            partial_eq,
            from_reflect_attrs,
            type_path_attrs,
            custom_where,
            no_field_bounds,
            idents,
        } = self;

        debug.merge(other.debug)?;
        hash.merge(other.hash)?;
        partial_eq.merge(other.partial_eq)?;
        from_reflect_attrs.merge(other.from_reflect_attrs)?;
        type_path_attrs.merge(other.type_path_attrs)?;

        Self::merge_custom_where(custom_where, other.custom_where);

        *no_field_bounds |= other.no_field_bounds;

        for ident in other.idents {
            add_unique_ident(idents, ident)?;
        }
        Ok(())
    }

    fn merge_custom_where(this: &mut Option<WhereClause>, other: Option<WhereClause>) {
        match (this, other) {
            (Some(this), Some(other)) => {
                this.predicates.extend(other.predicates);
            }
            (this @ None, Some(other)) => {
                *this = Some(other);
            }
            _ => {}
        }
    }
}

/// Adds an identifier to a vector of identifiers if it is not already present.
///
/// Returns an error if the identifier already exists in the list.
fn add_unique_ident(idents: &mut Vec<Ident>, ident: Ident) -> Result<(), syn::Error> {
    let ident_name = ident.to_string();
    if idents.iter().any(|i| i == ident_name.as_str()) {
        return Err(syn::Error::new(ident.span(), CONFLICTING_TYPE_DATA_MESSAGE));
    }

    idents.push(ident);
    Ok(())
}

/// Extract a boolean value from an expression.
///
/// The mapper exists so that the caller can conditionally choose to use the given
/// value or supply their own.
fn extract_bool(
    value: &Expr,
    mut mapper: impl FnMut(&LitBool) -> LitBool,
) -> Result<LitBool, syn::Error> {
    match value {
        Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Bool(lit),
            ..
        }) => Ok(mapper(lit)),
        _ => Err(syn::Error::new(value.span(), "Expected a boolean value")),
    }
}
