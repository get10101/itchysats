alter table orders
rename column term_seconds to settlement_time_interval_seconds;

alter table orders
drop column creation_timestamp_nanoseconds;

alter table orders
drop column term_nanoseconds;
