use std::marker::PhantomData;
use xtra_productivity::Actor;

trait Foo {}

impl Foo for i32 {}

#[derive(Actor)]
struct OneArgWithBound<T: Foo> {
    phantom: PhantomData<T>,
}

fn main() {}
