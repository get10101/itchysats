alter table orders
rename column term_seconds to settlement_time_interval_seconds;

alter table orders
rename column term_nanoseconds to settlement_time_interval_nanoseconds;
