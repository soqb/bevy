use crate::container_attributes::ReflectTraits;
use crate::field_attributes::{parse_field_attrs, ReflectFieldAttr};
use crate::fq_std::{FQAny, FQDefault, FQSend, FQSync};
use crate::utility::{members_to_serialization_denylist, WhereClauseOptions};
use crate::utility::members_to_serialization_denylist;
use crate::with_path::parse_path_no_leading_colon;
use bit_set::BitSet;
use quote::{quote, ToTokens};

use crate::{
    utility, REFLECT_ATTRIBUTE_NAME, REFLECT_VALUE_ATTRIBUTE_NAME, TYPE_PATH_ATTRIBUTE_NAME,
};
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{
    parse_str, Data, DeriveInput, Field, Fields, GenericParam, Generics, Ident, Lit, LitStr, Meta,
    Path, Token, Type, TypePath, Variant, ImplGenerics, TypeGenerics, WhereClause,
};

pub(crate) enum ReflectDerive<'a> {
    Struct(ReflectStruct<'a>),
    TupleStruct(ReflectStruct<'a>),
    UnitStruct(ReflectStruct<'a>),
    Enum(ReflectEnum<'a>),
    Value(ReflectMeta<'a>),
}

/// Metadata present on all reflected types, including name, generics, and attributes.
///
/// # Example
///
/// ```ignore
/// #[derive(Reflect)]
/// //                          traits
/// //        |----------------------------------------|
/// #[reflect(PartialEq, Serialize, Deserialize, Default)]
/// //            type_name       generics
/// //     |-------------------||----------|
/// struct ThingThatImReflecting<T1, T2, T3> {/* ... */}
/// ```
pub(crate) struct ReflectMeta<'a> {
    /// The registered traits for this type.
    traits: ReflectTraits,
    /// The name of this type.
    path_to_type: PathToType<'a>,
    /// The generics defined on this type.
    generics: &'a Generics,
    /// A cached instance of the path to the `bevy_reflect` crate.
    bevy_reflect_path: Path,
    /// The documentation for this type, if any
    #[cfg(feature = "documentation")]
    docs: crate::documentation::Documentation,
}

/// Struct data used by derive macros for `Reflect` and `FromReflect`.
///
/// # Example
///
/// ```ignore
/// #[derive(Reflect)]
/// #[reflect(PartialEq, Serialize, Deserialize, Default)]
/// struct ThingThatImReflecting<T1, T2, T3> {
///     x: T1, // |
///     y: T2, // |- fields
///     z: T3  // |
/// }
/// ```
pub(crate) struct ReflectStruct<'a> {
    meta: ReflectMeta<'a>,
    serialization_denylist: BitSet<u32>,
    fields: Vec<StructField<'a>>,
}

/// Enum data used by derive macros for `Reflect` and `FromReflect`.
///
/// # Example
///
/// ```ignore
/// #[derive(Reflect)]
/// #[reflect(PartialEq, Serialize, Deserialize, Default)]
/// enum ThingThatImReflecting<T1, T2, T3> {
///     A(T1),                  // |
///     B,                      // |- variants
///     C { foo: T2, bar: T3 }  // |
/// }
/// ```
pub(crate) struct ReflectEnum<'a> {
    meta: ReflectMeta<'a>,
    variants: Vec<EnumVariant<'a>>,
}

/// Represents a field on a struct or tuple struct.
pub(crate) struct StructField<'a> {
    /// The raw field.
    pub data: &'a Field,
    /// The reflection-based attributes on the field.
    pub attrs: ReflectFieldAttr,
    /// The index of this field within the struct.
    pub index: usize,
    /// The documentation for this field, if any
    #[cfg(feature = "documentation")]
    pub doc: crate::documentation::Documentation,
}

/// Represents a variant on an enum.
pub(crate) struct EnumVariant<'a> {
    /// The raw variant.
    pub data: &'a Variant,
    /// The fields within this variant.
    pub fields: EnumVariantFields<'a>,
    /// The reflection-based attributes on the variant.
    #[allow(dead_code)]
    pub attrs: ReflectFieldAttr,
    /// The index of this variant within the enum.
    #[allow(dead_code)]
    pub index: usize,
    /// The documentation for this variant, if any
    #[cfg(feature = "documentation")]
    pub doc: crate::documentation::Documentation,
}

pub(crate) enum EnumVariantFields<'a> {
    Named(Vec<StructField<'a>>),
    Unnamed(Vec<StructField<'a>>),
    Unit,
}

/// The method in which the type should be reflected.
#[derive(PartialEq, Eq)]
enum ReflectMode {
    /// Reflect the type normally, providing information about all fields/variants.
    Normal,
    /// Reflect the type as a value.
    Value,
}

impl<'a> ReflectDerive<'a> {
    pub fn from_input(input: &'a DeriveInput) -> Result<Self, syn::Error> {
        let mut traits = ReflectTraits::default();
        // Should indicate whether `#[reflect_value]` was used.
        let mut reflect_mode = None;
        // Should indicate whether `#[type_path = "..."]` was used.
        let mut type_path_alias: Option<Path> = None;

        #[cfg(feature = "documentation")]
        let mut doc = crate::documentation::Documentation::default();

        for attribute in input.attrs.iter().filter_map(|attr| attr.parse_meta().ok()) {
            match attribute {
                Meta::List(meta_list) if meta_list.path.is_ident(REFLECT_ATTRIBUTE_NAME) => {
                    if !matches!(reflect_mode, None | Some(ReflectMode::Normal)) {
                        return Err(syn::Error::new(
                            meta_list.span(),
                            format_args!("cannot use both `#[{REFLECT_ATTRIBUTE_NAME}]` and `#[{REFLECT_VALUE_ATTRIBUTE_NAME}]`"),
                        ));
                    }

                    reflect_mode = Some(ReflectMode::Normal);
                    let new_traits = ReflectTraits::from_nested_metas(&meta_list.nested)?;
                    traits = traits.merge(new_traits)?;
                }
                Meta::List(meta_list) if meta_list.path.is_ident(REFLECT_VALUE_ATTRIBUTE_NAME) => {
                    if !matches!(reflect_mode, None | Some(ReflectMode::Value)) {
                        return Err(syn::Error::new(
                            meta_list.span(),
                            format_args!("cannot use both `#[{REFLECT_ATTRIBUTE_NAME}]` and `#[{REFLECT_VALUE_ATTRIBUTE_NAME}]`"),
                        ));
                    }

                    reflect_mode = Some(ReflectMode::Value);
                    let new_traits = ReflectTraits::from_nested_metas(&meta_list.nested)?;
                    traits = traits.merge(new_traits)?;
                }
                Meta::Path(path) if path.is_ident(REFLECT_VALUE_ATTRIBUTE_NAME) => {
                    if !matches!(reflect_mode, None | Some(ReflectMode::Value)) {
                        return Err(syn::Error::new(
                            path.span(),
                            format_args!("cannot use both `#[{REFLECT_ATTRIBUTE_NAME}]` and `#[{REFLECT_VALUE_ATTRIBUTE_NAME}]`"),
                        ));
                    }

                    reflect_mode = Some(ReflectMode::Value);
                }
                Meta::NameValue(pair) if pair.path.is_ident(TYPE_PATH_ATTRIBUTE_NAME) => {
                    if type_path_alias.is_some() {
                        return Err(syn::Error::new(
                            pair.span(),
                            format_args!(
                                "cannot use multuple `#[{TYPE_PATH_ATTRIBUTE_NAME} = \"...\"]`"
                            ),
                        ));
                    }

                    let Lit::Str(lit) = pair.lit else {
                        return Err(syn::Error::new(
                            pair.span(),
                            format_args!("`#[{TYPE_PATH_ATTRIBUTE_NAME} = \"...\"]` must be a string literal"),
                        ));
                    };

                    type_path_alias = Some(syn::parse::Parser::parse_str(
                        parse_path_no_leading_colon,
                        &lit.value(),
                    )?);
                }
                #[cfg(feature = "documentation")]
                Meta::NameValue(pair) if pair.path.is_ident("doc") => {
                    if let Lit::Str(lit) = pair.lit {
                        doc.push(lit.value());
                    }
                }
                _ => continue,
            }
        }

        let path_to_type = if let Some(alias) = type_path_alias {
            PathToType::AliasedNamed {
                alias,
                ty: Type::Path(TypePath {
                    qself: None,
                    path: Path::from(input.ident.clone()),
                }),
            }
        } else {
            PathToType::Internal(&input.ident)
        };

        let meta = ReflectMeta::new(path_to_type, &input.generics, traits);

        #[cfg(feature = "documentation")]
        let meta = meta.with_docs(doc);

        // Use normal reflection if unspecified
        let reflect_mode = reflect_mode.unwrap_or(ReflectMode::Normal);

        if reflect_mode == ReflectMode::Value {
            return Ok(Self::Value(meta));
        }

        return match &input.data {
            Data::Struct(data) => {
                let fields = Self::collect_struct_fields(&data.fields)?;
                let reflect_struct = ReflectStruct {
                    meta,
                    serialization_denylist: members_to_serialization_denylist(
                        fields.iter().map(|v| v.attrs.ignore),
                    ),
                    fields,
                };

                match data.fields {
                    Fields::Named(..) => Ok(Self::Struct(reflect_struct)),
                    Fields::Unnamed(..) => Ok(Self::TupleStruct(reflect_struct)),
                    Fields::Unit => Ok(Self::UnitStruct(reflect_struct)),
                }
            }
            Data::Enum(data) => {
                let variants = Self::collect_enum_variants(&data.variants)?;

                let reflect_enum = ReflectEnum { meta, variants };
                Ok(Self::Enum(reflect_enum))
            }
            Data::Union(..) => Err(syn::Error::new(
                input.span(),
                "reflection not supported for unions",
            )),
        };
    }

    pub fn meta(&self) -> &ReflectMeta<'a> {
        match self {
            ReflectDerive::Struct(data) => data.meta(),
            ReflectDerive::TupleStruct(data) => data.meta(),
            ReflectDerive::UnitStruct(data) => data.meta(),
            ReflectDerive::Enum(data) => data.meta(),
            ReflectDerive::Value(meta) => meta,
        }
    }

    fn collect_struct_fields(fields: &'a Fields) -> Result<Vec<StructField<'a>>, syn::Error> {
        let sifter: utility::ResultSifter<StructField<'a>> = fields
            .iter()
            .enumerate()
            .map(|(index, field)| -> Result<StructField, syn::Error> {
                let attrs = parse_field_attrs(&field.attrs)?;
                Ok(StructField {
                    index,
                    attrs,
                    data: field,
                    #[cfg(feature = "documentation")]
                    doc: crate::documentation::Documentation::from_attributes(&field.attrs),
                })
            })
            .fold(
                utility::ResultSifter::default(),
                utility::ResultSifter::fold,
            );

        sifter.finish()
    }

    fn collect_enum_variants(
        variants: &'a Punctuated<Variant, Token![,]>,
    ) -> Result<Vec<EnumVariant<'a>>, syn::Error> {
        let sifter: utility::ResultSifter<EnumVariant<'a>> = variants
            .iter()
            .enumerate()
            .map(|(index, variant)| -> Result<EnumVariant, syn::Error> {
                let fields = Self::collect_struct_fields(&variant.fields)?;

                let fields = match variant.fields {
                    Fields::Named(..) => EnumVariantFields::Named(fields),
                    Fields::Unnamed(..) => EnumVariantFields::Unnamed(fields),
                    Fields::Unit => EnumVariantFields::Unit,
                };
                Ok(EnumVariant {
                    fields,
                    attrs: parse_field_attrs(&variant.attrs)?,
                    data: variant,
                    index,
                    #[cfg(feature = "documentation")]
                    doc: crate::documentation::Documentation::from_attributes(&variant.attrs),
                })
            })
            .fold(
                utility::ResultSifter::default(),
                utility::ResultSifter::fold,
            );

        sifter.finish()
    }
}

impl<'a> ReflectMeta<'a> {
    pub fn new(
        path_to_type: PathToType<'a>,
        generics: &'a Generics,
        traits: ReflectTraits,
    ) -> Self {
        Self {
            traits,
            path_to_type,
            generics,
            bevy_reflect_path: utility::get_bevy_reflect_path(),
            #[cfg(feature = "documentation")]
            docs: Default::default(),
        }
    }

    /// Sets the documentation for this type.
    #[cfg(feature = "documentation")]
    pub fn with_docs(self, docs: crate::documentation::Documentation) -> Self {
        Self { docs, ..self }
    }

    /// The registered reflect traits on this struct.
    pub fn traits(&self) -> &ReflectTraits {
        &self.traits
    }

    /// The name of this struct.
    pub fn path_to_type(&self) -> &'a PathToType {
        &self.path_to_type
    }

    /// The generics associated with this struct.
    pub fn generics(&self) -> &'a Generics {
        self.generics
    }

    /// The cached `bevy_reflect` path.
    pub fn bevy_reflect_path(&self) -> &Path {
        &self.bevy_reflect_path
    }
    
    /// Use this over [`Generics::split_for_impl`].
    pub fn split_generics_for_impl(&self) -> (ImplGenerics, Option<TypeGenerics>, Option<&WhereClause>) {
        let (impl_generics, ty_generics, where_clause) = self.generics.split_for_impl();
        if let PathToType::AliasedNamed { .. } = self.path_to_type {
            (impl_generics, None, where_clause)
        } else {
            (impl_generics, Some(ty_generics), where_clause)
        }
    }

    /// Whether an impl using this [`ReflectMeta`] should be generic.
    pub fn impl_is_generic(&self) -> bool {
        // Whether to use `GenericTypedCell` is not dependent on lifetimes
        // (which all have to be 'static anyway).
        !self
            .generics
            .params
            .iter()
            .all(|param| matches!(param, GenericParam::Lifetime(_)))
    }

    /// Returns the `GetTypeRegistration` impl as a `TokenStream`.
    pub fn get_type_registration(
        &self,
        where_clause_options: &WhereClauseOptions,
    ) -> proc_macro2::TokenStream {
        crate::registration::impl_get_type_registration(
            self.meta(),
            where_clause_options,
            None,
        )
    }

    /// The collection of docstrings for this type, if any.
    #[cfg(feature = "documentation")]
    pub fn doc(&self) -> &crate::documentation::Documentation {
        &self.docs
    }
}

impl<'a> ReflectStruct<'a> {
    /// Access the metadata associated with this struct definition.
    pub fn meta(&self) -> &ReflectMeta<'a> {
        &self.meta
    }

    /// Access the data about which fields should be ignored during serialization.
    ///
    /// The returned bitset is a collection of indices obtained from the [`members_to_serialization_denylist`](crate::utility::members_to_serialization_denylist) function.
    #[allow(dead_code)]
    pub fn serialization_denylist(&self) -> &BitSet<u32> {
        &self.serialization_denylist
    }

    /// Returns the `GetTypeRegistration` impl as a `TokenStream`.
    ///
    /// Returns a specific implementation for structs and this method should be preferred over the generic [`get_type_registration`](crate::ReflectMeta) method
    pub fn get_type_registration(
        &self,
        where_clause_options: &WhereClauseOptions,
    ) -> proc_macro2::TokenStream {
        crate::registration::impl_get_type_registration(
            self.meta(),
            where_clause_options,
            Some(&self.serialization_denylist),
        )
    }

    /// Get a collection of types which are exposed to the reflection API
    pub fn active_types(&self) -> Vec<syn::Type> {
        self.active_fields()
            .map(|field| field.data.ty.clone())
            .collect()
    }

    /// Get an iterator of fields which are exposed to the reflection API
    pub fn active_fields(&self) -> impl Iterator<Item = &StructField<'a>> {
        self.fields
            .iter()
            .filter(|field| field.attrs.ignore.is_active())
    }

    /// Get a collection of types which are ignored by the reflection API
    pub fn ignored_types(&self) -> Vec<syn::Type> {
        self.ignored_fields()
            .map(|field| field.data.ty.clone())
            .collect()
    }

    /// Get an iterator of fields which are ignored by the reflection API
    pub fn ignored_fields(&self) -> impl Iterator<Item = &StructField<'a>> {
        self.fields
            .iter()
            .filter(|field| field.attrs.ignore.is_ignored())
    }

    /// The complete set of fields in this struct.
    #[allow(dead_code)]
    pub fn fields(&self) -> &[StructField<'a>] {
        &self.fields
    }

    pub fn where_clause_options(&self) -> WhereClauseOptions {
        let bevy_reflect_path = &self.meta().bevy_reflect_path;
        WhereClauseOptions {
            active_types: self.active_types().into(),
            active_trait_bounds: quote! { #bevy_reflect_path::Reflect },
            ignored_types: self.ignored_types().into(),
            ignored_trait_bounds: quote! { #FQAny + #FQSend + #FQSync },
        }
    }
}

impl<'a> ReflectEnum<'a> {
    /// Access the metadata associated with this enum definition.
    pub fn meta(&self) -> &ReflectMeta<'a> {
        &self.meta
    }

    /// Returns the given ident as a qualified unit variant of this enum.
    pub fn get_unit(&self, variant: &Ident) -> proc_macro2::TokenStream {
        let name = self.meta.path_to_type();
        quote! {
            #name::#variant
        }
    }

    /// The complete set of variants in this enum.
    pub fn variants(&self) -> &[EnumVariant<'a>] {
        &self.variants
    }

    /// Get an iterator of fields which are exposed to the reflection API
    pub fn active_fields(&self) -> impl Iterator<Item = &StructField<'a>> {
        self.variants()
            .iter()
            .flat_map(|variant| variant.active_fields())
    }

    /// Get a collection of types which are exposed to the reflection API
    pub fn active_types(&self) -> Vec<syn::Type> {
        self.active_fields()
            .map(|field| field.data.ty.clone())
            .collect()
    }

    /// Get an iterator of fields which are ignored by the reflection API
    pub fn ignored_fields(&self) -> impl Iterator<Item = &StructField<'a>> {
        self.variants()
            .iter()
            .flat_map(|variant| variant.ignored_fields())
    }

    /// Get a collection of types which are ignored to the reflection API
    pub fn ignored_types(&self) -> Vec<syn::Type> {
        self.ignored_fields()
            .map(|field| field.data.ty.clone())
            .collect()
    }

    pub fn where_clause_options(&self) -> WhereClauseOptions {
        let bevy_reflect_path = &self.meta().bevy_reflect_path;
        WhereClauseOptions {
            active_types: self.active_types().into(),
            active_trait_bounds: quote! { #bevy_reflect_path::FromReflect },
            ignored_types: self.ignored_types().into(),
            ignored_trait_bounds: quote! { #FQAny + #FQSend + #FQSync + #FQDefault },
        }
    }
}

impl<'a> EnumVariant<'a> {
    /// Get an iterator of fields which are exposed to the reflection API
    #[allow(dead_code)]
    pub fn active_fields(&self) -> impl Iterator<Item = &StructField<'a>> {
        self.fields()
            .iter()
            .filter(|field| field.attrs.ignore.is_active())
    }

    /// Get an iterator of fields which are ignored by the reflection API
    #[allow(dead_code)]
    pub fn ignored_fields(&self) -> impl Iterator<Item = &StructField<'a>> {
        self.fields()
            .iter()
            .filter(|field| field.attrs.ignore.is_ignored())
    }

    /// The complete set of fields in this variant.
    #[allow(dead_code)]
    pub fn fields(&self) -> &[StructField<'a>] {
        match &self.fields {
            EnumVariantFields::Named(fields) | EnumVariantFields::Unnamed(fields) => fields,
            EnumVariantFields::Unit => &[],
        }
    }
}

pub(crate) enum PathToType<'a> {
    /// The name of a type relative to itself.
    ///
    /// Module and crate are found with [`module_path!()`](core::module_path).
    Internal(&'a Ident),
    /// Using `::my_crate::foo::Bar` syntax.
    External(&'a Path),
    /// Types without a crate/module (e.g. `bool`).
    Primtive(&'a Ident),
    /// [`PathToType::External`] but with a seperately defined qualified path.
    AliasedNamed { ty: Type, alias: Path },
    /// Types without a crate/module and with a seperately defined qualified path.
    AliasedAnonymous { ty: Type, alias: LitStr },
}

impl<'a> PathToType<'a> {
    /// Returns whether [`crate_name`](PathToType::crate_name)
    /// and [`module_path`](PathToType::module_path) return [`Some`].
    ///
    /// This is true for primitives and anonymous paths.
    pub fn has_module(&self) -> bool {
        match self {
            Self::Primtive(_) | Self::AliasedAnonymous { .. } => false,
            _ => true,
        }
    }

    /// Returns an expression for a [`&str`](str).
    pub fn crate_name(&self) -> Option<proc_macro2::TokenStream> {
        match self {
            Self::Internal { .. } => Some(quote! {
                ::core::module_path!()
                    .split(':')
                    .next()
                    .unwrap()
            }),
            &Self::External(&ref path) | Self::AliasedNamed { alias: ref path, .. } => {
                let crate_name = path.segments.first().unwrap().ident.to_string();
                let crate_name = LitStr::new(&crate_name, path.span());
                Some(quote!(#crate_name))
            }
            Self::Primtive(_) => None,
        }
    }

    /// Returns an expression representing a [`String`].
    pub fn fully_qualified_path(&self) -> proc_macro2::TokenStream {
        match self {
            Self::Internal(ident) => {
                let module_path = self.module_path().unwrap();
                let ident = LitStr::new(&ident.to_string(), ident.span());
                quote! {
                    #module_path.to_owned() + ::core::concat!("::", #ident)
                }
            }
            Self::External(&ref path) | Self::AliasedNamed { alias: ref path, .. } => {
                let module_path = self.module_path().unwrap();
                let ident = &path.segments.last().unwrap().ident;
                let ident = LitStr::new(&ident.to_string(), ident.span());
                quote! {
                    #module_path.to_owned() + ::core::concat!("::", #ident)
                }
            }
            Self::Primtive(ident) => {
                let ident = LitStr::new(&ident.to_string(), ident.span());
                quote! {
                    #ident.to_owned()
                }
            }
        }
    }

    /// Returns an expression for a [`&str`](str).
    pub fn module_path(&self) -> Option<proc_macro2::TokenStream> {
        match self {
            Self::Internal { .. } => Some(quote! {
                ::core::module_path!()
            }),
            Self::External(&ref path) | Self::AliasedNamed { alias: ref path, .. } => {
                let path = path
                    .segments
                    .pairs()
                    .take(path.segments.len() - 1)
                    .map(|pair| pair.value().ident.to_string())
                    .reduce(|path, ident| path + "::" + &ident)
                    .unwrap();

                let path = LitStr::new(&path, path.span());
                Some(quote! {
                    #path
                })
            }
            Self::Primtive(_) => None,
        }
    }

    /// Returns the name of the type. This is not necessarily a valid qualified path to the type.
    pub fn name(&self) -> &Ident {
        match self {
            Self::Internal(ident) | Self::Primtive(ident) => ident,
            Self::External(&ref path) | Self::AliasedNamed { alias: ref path, .. } => {
                &path.segments.last().unwrap().ident
            }
        }
    }
}

impl<'a> ToTokens for PathToType<'a> {
    fn to_tokens(&self, tokens: &mut proc_macro2::TokenStream) {
        match self {
            Self::Internal(ident) | Self::Primtive(ident) => ident.to_tokens(tokens),
            Self::External(path) => path.to_tokens(tokens),
            Self::AliasedNamed {
                ty,
                ..
            } | Self::AliasedAnonymous { ty, .. } => ty.to_tokens(tokens),
        }
    }
}

pub(crate) fn type_path_generator(meta: &ReflectMeta) -> proc_macro2::TokenStream {
    let path_to_type = meta.path_to_type();
    let generics = meta.generics();
    let bevy_reflect_path = meta.bevy_reflect_path();
    // Whether to use `GenericTypedCell` is not dependent on lifetimes
    // (which all have to be 'static anyway).
    let is_generic = !generics
        .params
        .iter()
        .all(|param| matches!(param, GenericParam::Lifetime(_)));
    let generic_type_paths: Vec<proc_macro2::TokenStream> = generics
        .type_params()
        .map(|param| {
            let ident = &param.ident;
            quote! {
                <#ident as #bevy_reflect_path::WithPath>::type_path()
            }
        })
        .collect();

    let ident = path_to_type.name().to_string();
    let ident = LitStr::new(&ident, path_to_type.span());

    let path = {
        let path = path_to_type.fully_qualified_path();

        if is_generic {
            let generics = generic_type_paths.iter().map(|type_path| {
                quote! {
                    #type_path.path()
                }
            });

            quote! {
                #path + "::<" #(+ #generics)* + ">"
            }
        } else {
            quote! {
                #path
            }
        }
    };

    let short_path = {
        if is_generic {
            let generics = generic_type_paths.iter().map(|type_path| {
                quote! {
                    #type_path.short_path()
                }
            });

            quote! {
                ::core::concat!(#ident, "<").to_owned() #(+ #generics)* + ">"
            }
        } else {
            quote! {
                #ident.to_owned()
            }
        }
    };

    if path_to_type.has_module() {
        quote! {
            #bevy_reflect_path::TypePath::new_anonymous(
                #path,
                #short_path,
            )
        }
    } else {
        let crate_name = path_to_type.crate_name();
        let module_path = path_to_type.module_path();

        quote! {
            #bevy_reflect_path::TypePath::new_named(
                #path,
                #short_path,
                #ident.to_owned(),
                #crate_name.to_owned(),
                #module_path.to_owned(),
            )
        }
    }
}
