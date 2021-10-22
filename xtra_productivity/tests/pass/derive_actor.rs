use std::marker::PhantomData;
use xtra::Actor;
use xtra_productivity::Actor;

#[derive(Actor)]
struct Simple;

#[derive(Actor)]
struct OneArg<T> {
    phantom: PhantomData<T>,
}

#[derive(Actor)]
struct TwoArgs<A, B> {
    phantom: PhantomData<(A, B)>,
}

trait Foo {}

impl Foo for i32 {}

#[derive(Actor)]
struct OneArgWithBound<T>
where
    T: Foo,
{
    phantom: PhantomData<T>,
}

fn assert_impl_actor<A: Actor>() {}

fn main() {
    assert_impl_actor::<Simple>();
    assert_impl_actor::<OneArg<i32>>();
    assert_impl_actor::<OneArgWithBound<i32>>();
    assert_impl_actor::<TwoArgs<i32, i64>>();
}
