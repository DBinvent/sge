extern crate postgres;
use postgres::{Client, NoTls};
use std::result::Result;

extern crate chrono;

#[macro_use]
extern crate log;

pub static TABLES: &'static [&'static str; 3] = & [
    "config", "pname varchar(40) primary key, pvalue varchar(240)",
    "RDBM configuration",
]

pub fn from_list(list: &'a [&str]) -> Result<Vec<Entity<'a>>, &'a str> {
    if list.len() == 0usize {
        Err("No data provided")
    } else if list.len() % 3usize != 0usize {
        Err("wrong data alignment")
    } else {
        let mut i: usize = 0;
        let mut r = Vec::new();
        while i + 3 <= list.len() {
            r.push(Entity::new(&list[i..i + 3])?);
            i += 3;
        }
        Ok(r)
    }
}


/// Simple DB migration tool to initialize database Table, without structure change but with new Table and schema apply
///``` match migrate(&ctx.cfg.db.value, &ctx.cfg.schema.value, dbm_lib::Entity::from_list(cfg::TABLES).unwrap()) {
pub fn migrate(db: &String, schema: &String, tables: Vec<Entity>) -> Result<u16, String> {
    let mut client = match Client::connect(db, NoTls) {
        Ok(client) => client,
        Err(err) => {
            return Err(format!(
                "ERROR on Connect to Postgres [{}]: \n{} ",
                db,
                err.to_string()
            ))
        }
    };
    let mut schema = schema.clone();
    if schema.len() > 0usize && schema.chars().last().unwrap() != '.' {
        schema.push('.');
    }
    let mut cnt: u16 = migrate_schema(&mut client, &schema)?;
    match tables.len() {
        0 => Err(String::from("No data provided")),
        _ => {
            for t in tables {
                cnt += t.migrate(&mut client, &schema)?;
            }
            Ok(cnt)
        }
    }
}

fn migrate_schema(db: &mut Client, schema: &String) -> Result<u16, String> {
    if schema.len() == 0usize {
        return Ok(0);
    }
    let mut s = schema.clone();
    if schema.chars().last().unwrap() == '.' {
        s = schema[0..schema.len() - 1].to_string();
    }
    let sql: &str = &format!(
        "SELECT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = '{}')",
        s
    )[..];
    return match db.query(sql, &[]) {
        Err(e) => Err(format!("ERROR on running:\n{} \n{}", sql, e.to_string())),
        Ok(result) => match result.first() {
            Some(row) => {
                let se: bool = row.get(0);
                if !se {
                    match db.execute(&format!("create schema {}", s)[..], &[]) {
                        Err(e) => Err(format!("ERROR or running:\n{} \n{}", sql, e.to_string())),
                        Ok(_) => {
                            info!("Created schema {}", schema);
                            Ok(1)
                        }
                    }
                } else {
                    info!("Schema exists {}", schema);
                    Ok(0)
                }
            }
            None => Err(format!("ERROR on running:\n{} None", sql)),
        },
    };
}

#[derive(Debug)]
pub struct Entity<'a> {
    pub table: &'a str,
    pub definition: &'a str,
    pub comment: &'a str,
}

impl<'a> Entity<'a> {
    /// must be array of 3: table name, definition and comments (empty string is ok)
    pub fn new(list: &'a [&'a str]) -> Result<Entity<'a>, &'a str> {
        if list.len() != 3usize {
            Err("wrong data alignment")
        } else if list[0].len() == 0usize || list[0].find(" ").unwrap_or(0usize) > 0usize {
            Err("wrong table name")
        } else if list[2].len() == 0usize {
            Err("wrong table definition")
        } else {
            Ok(Entity {
                table: list[0],
                definition: list[1],
                comment: list[2],
            })
        }
    }

    /// convert array of str(%3) to list of Entity
    pub fn from_list(list: &'a [&str]) -> Result<Vec<Entity<'a>>, &'a str> {
        if list.len() == 0usize {
            Err("No data provided")
        } else if list.len() % 3usize != 0usize {
            Err("wrong data alignment")
        } else {
            let mut i: usize = 0;
            let mut r = Vec::new();
            while i + 3 <= list.len() {
                r.push(Entity::new(&list[i..i + 3])?);
                i += 3;
            }
            Ok(r)
        }
    }

    fn migrate(&self, db: &mut Client, schema: &String) -> Result<u16, String> {
        debug!("migrating table on {}: \n{:?} ", schema, self);
        let sql: &str = &format!("SELECT count(*) FROM {}{}", schema, self.table)[..];
        return match db.query(sql, &[]) {
            Err(_) => {
                let sql: &str = &format!(
                    "create table {}{} ({})",
                    schema, self.table, self.definition
                )[..];
                match db.execute(sql, &[]) {
                    Err(e) => Err(format!("ERROR or run:\n{} \n{}", sql, e.to_string())),
                    Ok(_) => {
                        if self.comment.len() > 0usize {
                            match db.execute(
                                &format!(
                                    "COMMENT ON TABLE {}{} IS \'{}\'",
                                    schema, self.table, self.comment
                                )[..],
                                &[],
                            ) {
                                Ok(_) => debug!("table commented"),
                                Err(e) => error!("table comment error {}", e.to_string()),
                            }
                        }
                        info!("Created table {}{}", schema, self.table);
                        Ok(1)
                    }
                }
            }
            Ok(_) => {
                debug!("Table exists: {}{}", schema, self.table);
                Ok(0)
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use crate::Entity;

    #[test]
    fn entity_from_list() {
        let e = Entity::from_list(&["a", "b", "c"]).unwrap();
        assert_eq!(e.len(), 1);
    }

    #[test]
    fn entity_from_list_e0() {
        match Entity::from_list(&[]) {
            Ok(_) => assert!(false, "wrong alignment"),
            Err(_) => assert!(true),
        };
    }

    #[test]
    fn entity_from_list_e1() {
        match Entity::from_list(&[""]) {
            Ok(_) => assert!(false, "wrong alignment"),
            Err(_) => assert!(true),
        };
    }

    #[test]
    fn entity_from_list_e2() {
        match Entity::from_list(&["", "b", ""]) {
            Ok(_) => assert!(false, "wrong table"),
            Err(_) => assert!(true),
        };
    }

    #[test]
    fn entity_from_list_e3() {
        match Entity::from_list(&["a b", "c", ""]) {
            Ok(_) => assert!(false, "wrong table name"),
            Err(_) => assert!(true),
        };
    }
}
