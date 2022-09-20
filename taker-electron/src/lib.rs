use neon::prelude::*;
use once_cell::sync::OnceCell;
use taker::Opts;
use tokio::runtime::Runtime;

// Return a global tokio runtime or create one if it doesn't exist.
// Throws a JavaScript exception if the `Runtime` fails to create.
fn runtime<'a, C: Context<'a>>(cx: &mut C) -> NeonResult<&'static Runtime> {
    static RUNTIME: OnceCell<Runtime> = OnceCell::new();

    RUNTIME.get_or_try_init(|| Runtime::new().or_else(|err| cx.throw_error(err.to_string())))
}

/// Starts the itchysats taker daemon.
/// returns a `Promise`and executes asynchronously on the `tokio` thread pool
pub fn start(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let rt = runtime(&mut cx)?;
    let channel = cx.channel();

    // Create a JavaScript promise and a `deferred` handle for resolving it.
    // It is important to be careful not to perform failable actions after
    // creating the promise to avoid an unhandled rejection.
    let (deferred, promise) = cx.promise();

    let network = cx.argument::<JsString>(0)?.value(&mut cx);
    let data_dir = cx.argument::<JsString>(1)?.value(&mut cx);

    // Spawn an `async` task on the tokio runtime. Only Rust types that are
    // `Send` may be moved into this block. `Context` may not be passed and all
    // JavaScript values must first be converted to Rust types.
    //
    // This task will _not_ block the JavaScript main thread.
    rt.spawn(async move {
        // Inside this block, it is possible to `await` Rust `Future`
        let opts = Opts::new(network, data_dir).expect("valid options");
        let result = taker::run(opts).await;

        // Settle the promise from the result of a closure. JavaScript exceptions
        // will be converted to a Promise rejection.
        //
        // This closure will execute on the JavaScript main thread. It should be
        // limited to converting Rust types to JavaScript values. Expensive operations
        // should be performed outside of it.
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(_) => Ok(cx.undefined()),
            Err(e) => cx.throw_error(format!("error: {e}")),
        })
    });

    Ok(promise)
}

#[neon::main]
fn main(mut cx: ModuleContext) -> NeonResult<()> {
    cx.export_function("itchysats", start)?;
    Ok(())
}
