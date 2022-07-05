use crate::ActorName;
use crate::SendAsyncSafe;
use anyhow::Result;
use std::collections::hash_map::Entry;
use std::collections::hash_map::Keys;
use std::collections::HashMap;
use std::hash::Hash;
use xtra::Address;
use xtra::Handler;

pub struct AddressMap<K, A> {
    inner: HashMap<K, Address<A>>,
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

    /// Garbage-collect addresses that are no longer active.
    fn gc(&mut self) {
        self.inner.retain(|_, candidate| candidate.is_connected());
    }

    pub fn is_empty(&mut self) -> bool {
        self.gc();
        self.inner.is_empty()
    }

    pub fn len(&mut self) -> usize {
        self.gc();
        self.inner.len()
    }

    pub fn keys(&self) -> Keys<'_, K, Address<A>> {
        self.inner.keys()
    }

    pub fn insert(&mut self, key: K, address: Address<A>) {
        self.gc();
        self.inner.insert(key, address);
    }

    /// Sends a message to the actor stored with the given key.
    pub async fn send<M>(&self, key: &K, msg: M) -> Result<(), NotConnected>
    where
        A: Handler<M, Return = ()> + ActorName,
        M: Send + 'static,
    {
        self.get(key)?
            .send(msg)
            .await
            .map_err(|_| NotConnected::new::<A>())?;

        Ok(())
    }

    pub async fn send_async<M>(&self, key: &K, msg: M) -> Result<(), NotConnected>
    where
        A: Handler<M, Return = ()> + ActorName,
        M: Send + 'static,
    {
        self.get(key)?
            .send_async_safe(msg)
            .await
            .map_err(|_| NotConnected::new::<A>())?;

        Ok(())
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
pub struct NotConnected(pub String);

impl NotConnected {
    pub fn new<A>() -> Self
    where
        A: ActorName,
    {
        NotConnected(A::name())
    }
}

#[derive(thiserror::Error, Debug, Clone, Copy)]
#[error("The address is still connected")]
pub struct StillConnected;

pub struct Disconnected<'a, K, A> {
    entry: Entry<'a, K, Address<A>>,
}

impl<'a, K, A> Disconnected<'a, K, A> {
    pub fn insert(self, address: Address<A>) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio_extras::Tasks;
    use xtra::Context;

    #[tokio::test]
    async fn gc_removes_address_if_address_disconnects() {
        let mut tasks = Tasks::default();
        let mut map = AddressMap::default();
        let (addr_1, ctx_1) = Context::new(None);
        tasks.add(ctx_1.run(Dummy));
        map.insert("addr_1", addr_1.clone());

        addr_1.send(Shutdown).await.unwrap();
        tokio_extras::time::sleep(Duration::from_secs(2)).await;

        let (addr_2, _ctx_2) = Context::new(None);
        map.insert("addr_2", addr_2); // inserting another address should GC `addr_1`

        assert_eq!(map.inner.len(), 1);
        assert!(map.inner.get("addr_2").is_some());
    }

    struct Dummy;

    struct Shutdown;

    #[async_trait::async_trait]
    impl xtra::Actor for Dummy {
        type Stop = ();

        async fn stopped(self) -> Self::Stop {}
    }

    #[xtra_productivity::xtra_productivity]
    impl Dummy {
        fn handle_shutdown(&mut self, _: Shutdown, ctx: &mut Context<Self>) {
            ctx.stop_self()
        }
    }
}
