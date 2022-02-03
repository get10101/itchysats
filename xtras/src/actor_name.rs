use regex::Regex;
use xtra::Actor;

pub trait ActorName {
    fn name() -> String;
}

impl<T> ActorName for T
where
    T: Actor,
{
    /// Devise the name of an actor from its type on a best-effort
    /// basis.
    ///
    /// To reduce some noise, we strip the crate name out of all
    /// module names contained in the type.
    fn name() -> String {
        let name = std::any::type_name::<T>();

        let regex = Regex::new("^.+?::").expect("valid regex");
        regex.replace(name, "").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actor_name_from_type() {
        let name = Dummy::name();

        assert_eq!(name, "actor_name::tests::Dummy")
    }

    struct Dummy;

    impl xtra::Actor for Dummy {}
}
