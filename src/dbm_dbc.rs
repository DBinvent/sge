extern crate postgres;
use postgres::{Client, NoTls};
use std::result::Result;
use std::collections::{HashMap, HashSet, BTreeMap};
use serde::Deserialize;
use crate::dbm_yml::{ConfigMap};
use std::collections::hash_map::RandomState;


/// information schema types: schema, table, column
pub (crate) type IS_TYPE = HashMap<String, HashMap<String, STable>>;

#[derive(Debug, Clone)]
/// information schema data
pub (crate) struct STable {
    /// column name,
    pub columns: HashMap<String, String>,
    /// trigger name, trigger's schema
    pub triggers: HashMap<String, String>,
}

pub (crate) fn load_info_schema(db: &mut Client, cfg: &ConfigMap) -> Result<IS_TYPE, String> {
    let mut data = load_info_cc(db, cfg)?;
    let _ = load_info_tg(db, &mut data, cfg)?;
    Ok(data)
}

#[inline]
fn load_info_cc(db: &mut Client, cfg: &ConfigMap) -> Result<IS_TYPE, String> {
    let mut data = HashMap::new();
    match db.query("SELECT table_catalog, table_schema, table_name, column_name, column_default, data_type from information_schema.columns where table_schema not in ('pg_catalog', 'information_schema')", &[]) {
        Err(e) => Err(format!("ERROR on running information_schema: {}", e)),
        Ok(result) => {
            for r in result {
                let table_catalog: &str = r.get(0);
                let table_schema: &str = r.get(1);
                let table_name: &str = r.get(2);
                let column_name: &str = r.get(3);
                let column_default: &str = match r.try_get(4) {
                    Ok(t) => t,
                    Err(e) => "",
                };
                let data_type: &str = r.get(5);
                let column_data = format!("{} {}", data_type, column_default);
                match data.get_mut(table_schema) {
                    None => {
                        let mut hd = HashMap::new();
                        hd.insert(table_name.into(), STable::new(column_name, column_data));
                        data.insert(table_schema.into(), hd);
                    }
                    Some(s) => {
                        match s.get_mut(table_name) {
                            None => {
                                s.insert(table_name.into(), STable::new(column_name, column_data));
                            }
                            Some(hd) => {
                                hd.columns.insert(column_name.into(), column_data);
                            }
                        }
                    }
                }
            }
            Ok(data)
        }
    }
}

#[inline]
fn load_info_tg(db: &mut Client, data: &mut IS_TYPE, cfg: &ConfigMap) -> Result<(), String> {
    match db.query("SELECT trigger_catalog, trigger_schema, trigger_name, event_object_catalog, event_object_schema, event_object_table \
        from information_schema.triggers where event_object_schema not in ('pg_catalog', 'information_schema')", &[]) {
        Err(e) => Err(format!("ERROR on running information_schema.triggers: {}", e)),
        Ok(result) => {
            for r in result {
                let trigger_catalog: &str = r.get(0);
                let trigger_schema: &str = r.get(1);
                let trigger_name: &str = r.get(2);
                let event_object_catalog: &str = r.get(3);
                let event_object_schema: &str = r.get(4);
                let event_object_table: &str = r.get(5);
                let trigger_data = format!("{} {} {}", trigger_catalog, trigger_schema, event_object_catalog);
                match data.get_mut(event_object_schema) {
                    None => {
                        let mut hd = HashMap::new();
                        hd.insert(event_object_table.into(), STable::newt(trigger_name, trigger_data));
                        data.insert(event_object_schema.into(), hd);
                    }
                    Some(s) => {
                        match s.get_mut(event_object_table) {
                            None => {
                                s.insert(event_object_table.into(), STable::newt(trigger_name, trigger_data));
                            }
                            Some(hd) => {
                                hd.triggers.insert(trigger_name.into(), trigger_data);
                            }
                        }
                    }
                }
            }
            Ok(())
        }
    }
}

impl STable {
    #[inline]
    fn new(column: &str, column_data: String) -> Self {
        let mut cls = HashMap::new();
        cls.insert(column.into(), column_data);
        STable {
            columns: cls,
            triggers: HashMap::new(),
        }
    }

    #[inline]
    fn newt(trig: &str, trig_data: String) -> Self {
        let mut tgs = HashMap::new();
        tgs.insert(trig.into(), trig_data);
        STable {
           triggers: tgs,
           columns  : HashMap::new(),
        }
    }
}

