use crate::{FromType, PartialReflect, Reflect};

/// A trait for types which can be constructed from a reflected type.
///
/// This trait can be derived on types which implement [`Reflect`]. Some complex
/// types (such as `Vec<T>`) may only be reflected if their element types
/// implement this trait.
///
/// For structs and tuple structs, fields marked with the `#[reflect(ignore)]`
/// attribute will be constructed using the `Default` implementation of the
/// field type, rather than the corresponding field value (if any) of the
/// reflected value.
pub trait FromReflect: Reflect + Sized {
    /// Constructs a concrete instance of `Self` from a reflected value.
    fn from_reflect(reflect: &dyn PartialReflect) -> Option<Self>;

    /// Attempts to downcast the given value to `Self` using,
    /// constructing the value using [`from_reflect`] if that fails.
    ///
    /// This method is more efficient than using [`from_reflect`] for cases where
    /// the given value is likely a boxed instance of `Self` (i.e. `Box<Self>`)
    /// rather than a boxed dynamic type (e.g. [`DynamicStruct`], [`DynamicList`], etc.).
    ///
    /// [`from_reflect`]: Self::from_reflect
    /// [`DynamicStruct`]: crate::DynamicStruct
    /// [`DynamicList`]: crate::DynamicList
    fn take_from_reflect(
        reflect: Box<dyn PartialReflect>,
    ) -> Result<Self, Box<dyn PartialReflect>> {
        match reflect.into_full()?.take::<Self>() {
            Ok(value) => Ok(value),
            Err(value) => match Self::from_reflect(value.as_partial_reflect()) {
                None => Err(value.into_partial_reflect()),
                Some(value) => Ok(value),
            },
        }
    }
}

/// Type data that represents the [`FromReflect`] trait and allows it to be used dynamically.
///
/// `FromReflect` allows dynamic types (e.g. [`DynamicStruct`], [`DynamicEnum`], etc.) to be converted
/// to their full, concrete types. This is most important when it comes to deserialization where it isn't
/// guaranteed that every field exists when trying to construct the final output.
///
/// However, to do this, you normally need to specify the exact concrete type:
///
/// ```
/// # use bevy_reflect::{DynamicTupleStruct, FromReflect, Reflect};
/// #[derive(Reflect, FromReflect, PartialEq, Eq, Debug)]
/// struct Foo(#[reflect(default = "default_value")] usize);
///
/// fn default_value() -> usize { 123 }
///
/// let reflected = DynamicTupleStruct::default();
///
/// let concrete: Foo = <Foo as FromReflect>::from_reflect(&reflected).unwrap();
///
/// assert_eq!(Foo(123), concrete);
/// ```
///
/// In a dynamic context where the type might not be known at compile-time, this is nearly impossible to do.
/// That is why this type data struct exists— it allows us to construct the full type without knowing
/// what the actual type is.
///
/// # Example
///
/// ```
/// # use bevy_reflect::{DynamicTupleStruct, FromReflect, Reflect, ReflectFromReflect, TypeRegistry};
/// # #[derive(Reflect, FromReflect, PartialEq, Eq, Debug)]
/// # #[reflect(FromReflect)]
/// # struct Foo(#[reflect(default = "default_value")] usize);
/// # fn default_value() -> usize { 123 }
/// # let mut registry = TypeRegistry::new();
/// # registry.register::<Foo>();
///
/// let mut reflected = DynamicTupleStruct::default();
/// reflected.set_name(std::any::type_name::<Foo>().to_string());
///
/// let registration = registry.get_with_name(reflected.type_name()).unwrap();
/// let rfr = registration.data::<ReflectFromReflect>().unwrap();
///
/// let concrete: Box<dyn PartialReflect> = rfr.from_reflect(&reflected).unwrap();
///
/// assert_eq!(Foo(123), concrete.take::<Foo>().unwrap());
/// ```
///
/// [`DynamicStruct`]: crate::DynamicStruct
/// [`DynamicEnum`]: crate::DynamicEnum
#[derive(Clone)]
pub struct ReflectFromReflect {
    from_reflect: fn(&dyn PartialReflect) -> Option<Box<dyn PartialReflect>>,
}

impl ReflectFromReflect {
    /// Perform a [`FromReflect::from_reflect`] conversion on the given reflection object.
    ///
    /// This will convert the object to a concrete type if it wasn't already, and return
    /// the value as `Box<dyn PartialReflect>`.
    #[allow(clippy::wrong_self_convention)]
    pub fn from_reflect(
        &self,
        reflect_value: &dyn PartialReflect,
    ) -> Option<Box<dyn PartialReflect>> {
        (self.from_reflect)(reflect_value)
    }
}

impl<T: FromReflect> FromType<T> for ReflectFromReflect {
    fn from_type() -> Self {
        Self {
            from_reflect: |reflect_value| {
                T::from_reflect(reflect_value)
                    .map(|value| Box::new(value) as Box<dyn PartialReflect>)
            },
        }
    }
}
