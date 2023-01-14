use bevy_reflect::PartialReflect;

#[derive(Reflect)]
struct Foo<'a> {
    #[reflect(ignore)]
    value: &'a str,
}

fn main() {}
