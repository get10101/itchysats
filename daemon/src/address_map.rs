use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::hash::Hash;

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
