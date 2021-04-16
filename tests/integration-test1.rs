extern crate rdbm;

// #[macro_use]
// extern crate slog;

// use crate::rdbm::dbm_dbc::STable;

// use rdbm::dbm_yml::ConfigMap;

#[test]
fn integration_connect_test() {
    /*
       let mut cfg = ConfigMap::new("db_port", "5433");
       cfg.set("db_name", "vk");
       cfg.set("db_user", "vk");
       match connect("db", &cfg) {
           Ok(mut c) => {
               let dbc = crate::dbm_dbc::load_info_schema(&mut c, &cfg).unwrap();
               assert!(dbc.len() > 0);
               match dbc.get("public") {
                   None => { assert!(false); }
                   Some(p) => {
                       assert!(p.len() > 0);
                   }
               }
               println!("{:?}", dbc);
           }
           Err(e) => {
               assert!(false, "{}", e);
           }
       }

    */
}
