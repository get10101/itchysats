use future_ext::FutureExt;
use futures::future::RemoteHandle;
use futures::Future;

mod future_ext;

/// Struct controlling the lifetime of the async tasks, such as
/// running actors and periodic notifications. If it gets dropped, all
/// tasks are cancelled.
#[derive(Default)]
pub struct Tasks(Vec<RemoteHandle<()>>);

impl Tasks {
    /// Spawn the task on the runtime and remember the handle.
    ///
    /// The task will be stopped if this instance of [`Tasks`] goes
    /// out of scope.
    #[allow(clippy::dbg_macro)]
    pub fn add(&mut self, f: impl Future<Output = ()> + Send + 'static, name: &str) {
        let handle = f.spawn_with_handle();

        dbg!(name);
        let size_of_handle = std::mem::size_of::<RemoteHandle<()>>();
        dbg!(size_of_handle);
        let size_of_var = std::mem::size_of_val(&handle);
        dbg!(size_of_var);
        self.0.push(handle);
        dbg!(self.0.len());
        // let size_of_vec = std::mem::size_of_val(&*self.0);
        // dbg!(size_of_vec);
    }

    /// Spawn a fallible task on the runtime and remember the handle.
    ///
    /// The task will be stopped if this instance of [`Tasks`] goes
    /// out of scope. If the task fails, the `err_handler` will be
    /// invoked.
    #[allow(clippy::dbg_macro)]
    pub fn add_fallible<E, EF>(
        &mut self,
        f: impl Future<Output = Result<(), E>> + Send + 'static,
        err_handler: impl FnOnce(E) -> EF + Send + 'static,
        name: &str,
    ) where
        E: Send + 'static,
        EF: Future<Output = ()> + Send + 'static,
    {
        let fut = async move {
            match f.await {
                Ok(()) => {}
                Err(err) => err_handler(err).await,
            }
        };

        let handle = fut.spawn_with_handle();

        // let size_of_handle = std::mem::size_of::<RemoteHandle<()>>();
        // dbg!(size_of_handle);
        // let size_of_var = std::mem::size_of_val(&handle);
        // dbg!(size_of_var);
        self.0.push(handle);
        dbg!(self.0.len());
        dbg!(name);
        // let size_of_vec = std::mem::size_of_val(&*self.0);
        // dbg!(size_of_vec);
    }
}

#[cfg(feature = "xtra")]
impl xtra::spawn::Spawner for Tasks {
    fn spawn<F: Future<Output = ()> + Send + 'static>(&mut self, fut: F) {
        self.add(fut, "spawned");
    }
}
