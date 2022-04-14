use crate::projection::Cfd;
use model::Position;
use model::Usd;
use rust_decimal::prelude::ToPrimitive;
use std::collections::HashMap;

const DIRECTION_LABEL: &str = "direction";
const DIRECTION_LONG_LABEL: &str = "long";
const DIRECTION_SHORT_LABEL: &str = "short";
const STATUS_LABEL: &str = "status";
const STATUS_OPEN_LABEL: &str = "open";
const STATUS_CLOSED_LABEL: &str = "closed";
const STATUS_FAILED_LABEL: &str = "failed";

static POSITION_SIZE_GAUGE: conquer_once::Lazy<prometheus::GaugeVec> =
    conquer_once::Lazy::new(|| {
        prometheus::register_gauge_vec!(
            "position_size_quantity",
            "Total quantity of positions on itchsyats.",
            &[DIRECTION_LABEL, STATUS_LABEL]
        )
        .unwrap()
    });

pub(crate) fn update_position_metrics(cfds: &[Cfd]) {
    {
        let open_cfds = cfds.iter().filter(|cfd| cfd.is_open());
        let (open_long, open_short): (Vec<_>, Vec<_>) =
            open_cfds.partition(|cfd| cfd.position == Position::Long);

        POSITION_SIZE_GAUGE
            .with(&HashMap::from([
                (DIRECTION_LABEL, DIRECTION_LONG_LABEL),
                (STATUS_LABEL, STATUS_OPEN_LABEL),
            ]))
            .set(
                sum_amounts(&open_long)
                    .into_decimal()
                    .to_f64()
                    .unwrap_or_default(),
            );
        POSITION_SIZE_GAUGE
            .with(&HashMap::from([
                (DIRECTION_LABEL, DIRECTION_SHORT_LABEL),
                (STATUS_LABEL, STATUS_OPEN_LABEL),
            ]))
            .set(
                sum_amounts(&open_short)
                    .into_decimal()
                    .to_f64()
                    .unwrap_or_default(),
            );
    }

    {
        let closed_cfds = cfds.iter().filter(|cfd| cfd.is_closed());
        let (closed_long, closed_short): (Vec<_>, Vec<_>) =
            closed_cfds.partition(|cfd| cfd.position == Position::Long);

        POSITION_SIZE_GAUGE
            .with(&HashMap::from([
                (DIRECTION_LABEL, DIRECTION_LONG_LABEL),
                (STATUS_LABEL, STATUS_CLOSED_LABEL),
            ]))
            .set(
                sum_amounts(&closed_long)
                    .into_decimal()
                    .to_f64()
                    .unwrap_or_default(),
            );
        POSITION_SIZE_GAUGE
            .with(&HashMap::from([
                (DIRECTION_LABEL, DIRECTION_SHORT_LABEL),
                (STATUS_LABEL, STATUS_CLOSED_LABEL),
            ]))
            .set(
                sum_amounts(&closed_short)
                    .into_decimal()
                    .to_f64()
                    .unwrap_or_default(),
            );
    }
    {
        let failed_cfds = cfds.iter().filter(|cfd| cfd.is_failed());
        let (failed_long, failed_short): (Vec<_>, Vec<_>) =
            failed_cfds.partition(|cfd| cfd.position == Position::Long);

        POSITION_SIZE_GAUGE
            .with(&HashMap::from([
                (DIRECTION_LABEL, DIRECTION_LONG_LABEL),
                (STATUS_LABEL, STATUS_FAILED_LABEL),
            ]))
            .set(
                sum_amounts(&failed_long)
                    .into_decimal()
                    .to_f64()
                    .unwrap_or_default(),
            );
        POSITION_SIZE_GAUGE
            .with(&HashMap::from([
                (DIRECTION_LABEL, DIRECTION_SHORT_LABEL),
                (STATUS_LABEL, STATUS_FAILED_LABEL),
            ]))
            .set(
                sum_amounts(&failed_short)
                    .into_decimal()
                    .to_f64()
                    .unwrap_or_default(),
            );
    }
}

fn sum_amounts(cfds: &[&Cfd]) -> Usd {
    cfds.iter()
        .fold(Usd::ZERO, |sum, cfd| cfd.quantity_usd + sum)
}
