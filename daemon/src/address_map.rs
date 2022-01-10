use anyhow::Result;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::hash::Hash;
use xtra::Address;
use xtra::Handler;
use xtra::Message;

pub struct AddressMap<K, A> {
    inner: HashMap<K, xtra::Address<A>>,
}

impl<K, A> Default for AddressMap<K, A> {
    fn default() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }
}

impl<K, A> AddressMap<K, A>
where
    K: Eq + Hash,
{
    pub fn get_disconnected(&mut self, key: K) -> Result<Disconnected<'_, K, A>, StillConnected> {
        let entry = self.inner.entry(key);

        if matches!(entry, Entry::Occupied(ref occupied) if occupied.get().is_connected()) {
            return Err(StillConnected);
        }

        Ok(Disconnected { entry })
    }

    pub fn get_connected(&self, key: &K) -> Option<&xtra::Address<A>> {
        match self.inner.get(key) {
            Some(addr) if addr.is_connected() => Some(addr),
            _ => None,
        }
    }

    /// Garbage-collect an address that is no longer active.
    pub fn gc(&mut self, stopping: Stopping<A>) {
        self.inner.retain(|_, candidate| stopping.me != *candidate);
    }

    pub fn insert(&mut self, key: K, address: Address<A>) {
        self.inner.insert(key, address);
    }

    /// Sends a message to the actor stored with the given key.
    pub async fn send<M>(&self, key: &K, msg: M) -> Result<(), NotConnected>
    where
        M: Message<Result = ()>,
        A: Handler<M> + ActorName,
    {
        self.get(key)?
            .send(msg)
            .await
            .map_err(|_| NotConnected::new::<A>())?;

        Ok(())
    }

    /// Sends a message to the actor stored with the given key.
    pub async fn send_fallible<M>(&self, key: &K, msg: M) -> Result<Result<()>, NotConnected>
    where
        M: Message<Result = anyhow::Result<()>>,
        A: Handler<M> + ActorName,
    {
        let result = self
            .get(key)?
            .send(msg)
            .await
            .map_err(|_| NotConnected::new::<A>())?;

        Ok(result)
    }

    fn get(&self, key: &K) -> Result<&Address<A>, NotConnected>
    where
        A: ActorName,
    {
        self.inner.get(key).ok_or_else(|| NotConnected::new::<A>())
    }
}

#[derive(thiserror::Error, Debug)]
#[error("{0} actor is down")]
pub struct NotConnected(String);

impl NotConnected {
    pub fn new<A>() -> Self
    where
        A: ActorName,
    {
        NotConnected(A::actor_name())
    }
}

/// A message to notify that an actor instance is stopping.
pub struct Stopping<A> {
    pub me: Address<A>,
}

impl<A> Message for Stopping<A>
where
    A: 'static,
{
    type Result = ();
}

#[derive(thiserror::Error, Debug)]
#[error("The address is still connected")]
pub struct StillConnected;

pub struct Disconnected<'a, K, A> {
    entry: Entry<'a, K, xtra::Address<A>>,
}

impl<'a, K, A> Disconnected<'a, K, A> {
    pub fn insert(self, address: xtra::Address<A>) {
        match self.entry {
            Entry::Occupied(mut occ) => {
                occ.insert(address);
            }
            Entry::Vacant(vacc) => {
                vacc.insert(address);
            }
        };
    }
}

pub trait ActorName {
    fn actor_name() -> String;
}

#[cfg(test)]
mod tests {
    use super::*;
    use xtra::Context;

    #[test]
    fn gc_removes_address() {
        let (addr_1, _) = Context::<Dummy>::new(None);
        let (addr_2, _) = Context::<Dummy>::new(None);
        let mut map = AddressMap::default();
        map.insert("foo", addr_1);
        map.insert("bar", addr_2.clone());

        map.gc(Stopping { me: addr_2 });

        assert_eq!(map.inner.len(), 1);
        assert!(map.inner.get("foo").is_some());
    }

    struct Dummy;

    impl xtra::Actor for Dummy {}
}
