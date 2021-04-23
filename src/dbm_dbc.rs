extern crate postgres;
use crate::dbm_yml::ConfigMap;
use postgres::Client;
use serde::Serialize;
use std::collections::HashMap;
use std::result::Result;
use self::postgres::{Error, Row};

/// information schema types: schema, table, column
pub(crate) type InfoSchemaType = HashMap<String, HashMap<String, STable>>;
/// information schema types: schema, (owner, table: name: owner)
pub(crate) type InfoSchemaOwnerType = HashMap<String, (String, HashMap<String, String>)>;

#[derive(Debug, Clone, Serialize)]
/// information schema data
pub(crate) struct STable {
    /// column name,
    pub columns: HashMap<String, String>,
    /// column name, foreign schema, table, column, fk name
    pub fks: HashMap<String, FKTable>,
    /// trigger name, trigger's schema
    pub triggers: HashMap<String, String>,
}

/// FK information loaded from DB
#[derive(Debug, Clone, Serialize)]
pub(crate) struct FKTable {
    pub schema: String,
    pub table: String,
    pub column: String,
    pub name: String,
    pub sql: String,
}

pub(crate) fn load_info_schema(db: &mut Client) -> Result<InfoSchemaType, String> {
    let mut data = load_info_cc(db)?;
    let _ = load_info_fk(db, &mut data)?;
    let _ = load_info_tg(db, &mut data)?;
    Ok(data)
}

#[inline]
fn load_info_cc(db: &mut Client) -> Result<InfoSchemaType, String> {
    let mut data = HashMap::new();
    match db.query("SELECT table_catalog, table_schema, table_name, column_name, column_default, data_type from information_schema.columns where table_schema not in ('pg_catalog', 'information_schema')", &[]) {
        Err(e) => Err(format!("on loading information_schema: {}", e)),
        Ok(result) => {
            for r in result {
                let _table_catalog: &str = r.get(0);
                let table_schema: &str = r.get(1);
                let table_name: &str = r.get(2);
                let column_name: &str = r.get(3);
                let column_default: &str = match r.try_get(4) {
                    Ok(t) => t,
                    Err(_) => "",
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
fn load_info_tg(
    db: &mut Client,
    data: &mut InfoSchemaType,
) -> Result<(), String> {
    match db.query("SELECT trigger_catalog, trigger_schema, trigger_name, event_object_catalog, event_object_schema, event_object_table \
        from information_schema.triggers where event_object_schema not in ('pg_catalog', 'information_schema')", &[]) {
        Err(e) => Err(format!("on loading information_schema.triggers: {}", e)),
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

#[inline]
fn load_info_fk(
    db: &mut Client,
    data: &mut InfoSchemaType,
) -> Result<(), String> {
    match db.query("SELECT  tc.table_schema,  tc.table_name, kcu.column_name,
 ccu.table_schema AS foreign_schema_name, ccu.table_name AS foreign_table_name, ccu.column_name AS foreign_column_name, tc.constraint_name
 FROM information_schema.table_constraints AS tc
 JOIN information_schema.key_column_usage AS kcu ON tc.constraint_name = kcu.constraint_name
 JOIN information_schema.constraint_column_usage AS ccu ON ccu.constraint_name = tc.constraint_name
 WHERE constraint_type = 'FOREIGN KEY' ", &[]) {
        Err(e) => Err(format!("on loading information_schema.fk: {}", e)),
        Ok(result) => {
            for r in result {
                let table_schema: &str = r.get(0);
                let table_name: &str = r.get(1);
                let column_name: &str = r.get(2);
                let foreign_schema_name: &str = r.get(3);
                let foreign_table_name: &str = r.get(4);
                let foreign_column_name: &str = r.get(5);
                let constraint_name: &str = r.get(6);
                let fk = FKTable {
                    schema: foreign_schema_name.to_string(),
                    table: foreign_table_name.to_string(),
                    column: foreign_column_name.to_string(),
                    name: constraint_name.to_string(),
                    sql: "".to_string()
                };
                match data.get_mut(table_schema) {
                    None => {
                        let mut hd = HashMap::new();
                        hd.insert(table_name.into(), STable::newf(column_name, fk));
                        data.insert(table_schema.into(), hd);
                    }
                    Some(s) => {
                        match s.get_mut(table_name) {
                            None => {
                                // this should never happens because table must exist for the FK
                                s.insert(table_name.into(), STable::newf(column_name, fk));
                            }
                            Some(hd) => {
                                hd.fks.insert(column_name.into(), fk);
                            }
                        }
                    }
                }
            }
            Ok(())
        }
    }
}

#[inline]
pub(crate) fn load_info_schema_owner(db: &mut Client) -> Result<InfoSchemaOwnerType, String> {
    let mut res = HashMap::new();
    match db.query("select schema_name, schema_owner from information_schema.schemata where schema_name not in ('information_schema', 'pg_catalog')", &[]) {
        Ok(schemas) => {
            for schema in schemas {
                let schema_name: &str = schema.get(0);
                let schema_owner: &str = schema.get(1);
                let mut tables = HashMap::new();
                match db.query("select t.table_name, u.usename
from information_schema.tables t
join pg_catalog.pg_class c on (t.table_name = c.relname)
join pg_catalog.pg_user u on (c.relowner = u.usesysid)
where t.table_schema = $1 ", &[&schema_name]) {
                    Err(e) => { return Err(format!("on loading information_schema.fk: {}", e)); },
                    Ok(result) => {
                        for r in result {
                            let table_name: &str = r.get(0);
                            let table_owner: &str = r.get(1);
                            tables.insert(table_name.into(), table_owner.into());
                        }
                    }
                }
                res.insert(schema_name.into(), (schema_owner.into(), tables));
            }
            Ok(res)
        }
        Err(e) => Err(format!("on loading information schema owners: {}", e)),
    }
}

impl STable {
    /// pre loaded on information_schema.columns
    #[inline]
    fn new(column: &str, column_data: String) -> Self {
        let mut cls = HashMap::new();
        cls.insert(column.into(), column_data);
        STable {
            columns: cls,
            fks: HashMap::new(),
            triggers: HashMap::new(),
        }
    }

    /// loaded by SQL call from information_schema.table_constraints
    #[inline]
    fn newf(column: &str, fk: FKTable) -> Self {
        let mut fks = HashMap::new();
        fks.insert(column.into(), fk);
        STable {
            columns: HashMap::new(),
            fks,
            triggers: HashMap::new(),
        }
    }

    #[inline]
    fn newt(trig: &str, trig_data: String) -> Self {
        let mut tgs = HashMap::new();
        tgs.insert(trig.into(), trig_data);
        STable {
            triggers: tgs,
            fks: HashMap::new(),
            columns: HashMap::new(),
        }
    }
}
