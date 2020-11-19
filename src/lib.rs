mod dbm;

/**
##Description:
Relation (PostgreSQL) DB Migration Management tool (inspired by flyway while it was free)

Library Key Features:

1. Migrate (upgrade)
2. Downgrade (optional)
3. Re-runnable with tag tracking
4. Test: migrate, downgrade, rerun
5. Dry run (optional)
6. psql environment for script run


runs by tests
*/

//////////// unit tests
#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
