use daemon::bdk::bitcoin;
use daemon::seed;
use daemon::seed::RandomSeed;
use daemon::seed::Seed;
use daemon_tests::open_cfd;
use daemon_tests::start_both;
use daemon_tests::OpenCfdArgs;
use otel_tests::otel_test;
use rand::distributions::Alphanumeric;
use rand::thread_rng;
use rand::Rng;
use std::env;
use std::path::Path;

#[otel_test]
async fn fail_to_import_seed_with_open_cfds() {
    let (mut maker, mut taker) = start_both().await;

    let cfd_args = OpenCfdArgs::default();
    open_cfd(&mut taker, &mut maker, cfd_args.clone()).await;

    let random_dir: String = thread_rng()
        .sample_iter(&Alphanumeric)
        .take(7)
        .map(char::from)
        .collect();
    let data_dir = env::temp_dir().join(Path::new(&random_dir));
    tokio::fs::create_dir_all(&data_dir)
        .await
        .expect("failed to create random temp folder");

    taker
        .system
        .import_seed(
            RandomSeed::default().seed(),
            data_dir,
            bitcoin::Network::Testnet,
            seed::TAKER_WALLET_SEED_FILE,
        )
        .await
        .expect_err("import seed should be rejected as open CFDs exist");
}
