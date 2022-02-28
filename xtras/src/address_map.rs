use crate::ActorName;
use anyhow::Result;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::hash::Hash;
use xtra::Address;
use xtra::Handler;
use xtra::Message;

pub struct AddressMap<K, A> {
    inner: HashMap<K, Address<A>>,
}

/// A loud trait that makes sure we don't forget to return `StopAll` from the `stopping` lifecycle
/// callback.
///
/// Things might be outdated when you are reading this so bear that in mind.
/// There is an open patch to `xtra` that changes the default implementation of an actor's
/// `stopping` function to `StopSelf`. This is necessary, otherwise the supervisor implementation
/// provided in this crate does not work correctly. At the same time though, returning `StopSelf`
/// has another side-effect: It does not mark an address as disconnected if its only instance stops
/// with a return value of `StopSelf`.
///
/// The GC mechanism of the [`AddressMap`] only works if [`Address::is_connected`] properly returns
/// `false`. This trait is meant to remind users that we need to check this.
///
/// Once the bug in xtra is fixed, we can remove it again.
pub trait IPromiseIamReturningStopAllFromStopping {}

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
    A: IPromiseIamReturningStopAllFromStopping,
{
    pub fn get_disconnected(&mut self, key: K) -> Result<Disconnected<'_, K, A>, StillConnected> {
        let entry = self.inner.entry(key);

        if matches!(entry, Entry::Occupied(ref occupied) if occupied.get().is_connected()) {
            return Err(StillConnected);
        }

        Ok(Disconnected { entry })
    }

    pub fn get_connected(&self, key: &K) -> Option<&Address<A>> {
        match self.inner.get(key) {
            Some(addr) if addr.is_connected() => Some(addr),
            _ => None,
        }
    }

    /// Garbage-collect addresses that are no longer active.
    fn gc(&mut self) {
        self.inner.retain(|_, candidate| candidate.is_connected());
    }

    pub fn insert(&mut self, key: K, address: Address<A>) {
        self.gc();
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
        M: Message<Result = Result<()>>,
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
    use tokio_tasks::Tasks;
    use xtra::Context;
    use xtra::KeepRunning;

    #[tokio::test]
    async fn gc_removes_address_if_actor_returns_stop_all() {
        let mut tasks = Tasks::default();
        let mut map = AddressMap::default();
        let (addr_1, ctx_1) = Context::new(None);
        tasks.add(ctx_1.run(Dummy));
        map.insert("addr_1", addr_1.clone());

        addr_1.send(Shutdown).await.unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;

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

        async fn stopping(&mut self, _: &mut Context<Self>) -> KeepRunning {
            KeepRunning::StopAll
        }

        async fn stopped(self) -> Self::Stop {}
    }

    impl IPromiseIamReturningStopAllFromStopping for Dummy {}

    #[xtra_productivity::xtra_productivity]
    impl Dummy {
        fn handle_shutdown(&mut self, _: Shutdown, ctx: &mut Context<Self>) {
            ctx.stop()
        }
    }
}
