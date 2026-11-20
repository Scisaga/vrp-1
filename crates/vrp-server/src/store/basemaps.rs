use super::*;
use std::path::PathBuf;
use vrp_proto::maps::v1 as maps;

impl Store {
    pub(crate) fn init_basemaps(&mut self) -> Result<()> {
        let (bytes, count): (u64, u64) = self.conn.query_row(
            "SELECT coalesce(sum(length(meta)),0),count(*) FROM basemap_packages",
            [],
            |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64)),
        )?;
        self.payload_bytes += bytes;
        self.records += count;
        let (bytes, count): (u64, u64) = self.conn.query_row(
            "SELECT coalesce(sum(length(context)),0),count(*) FROM basemap_refs",
            [],
            |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64)),
        )?;
        self.payload_bytes += bytes;
        self.records += count;
        Ok(())
    }
    pub(crate) fn basemaps(&self) -> Result<Vec<maps::BasemapPackage>> {
        let mut statement = self
            .conn
            .prepare("SELECT meta FROM basemap_packages ORDER BY id")?;
        let rows = statement.query_map([], |r| r.get::<_, Vec<u8>>(0))?;
        rows.map(|r| Ok(maps::BasemapPackage::decode(r?.as_slice())?))
            .collect()
    }
    pub(crate) fn basemap(&self, id: &str) -> Result<(maps::BasemapPackage, PathBuf)> {
        let row: Option<(Vec<u8>, String)> = self
            .conn
            .query_row(
                "SELECT meta,path FROM basemap_packages WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (bytes, path) =
            row.ok_or_else(|| AppError::new("not_found", "basemap package not found"))?;
        Ok((maps::BasemapPackage::decode(bytes.as_slice())?, path.into()))
    }
    pub(crate) fn put_basemap(&mut self, p: &maps::BasemapPackage, path: &Path) -> Result<()> {
        let old: Option<u64> = self
            .conn
            .query_row(
                "SELECT length(meta) FROM basemap_packages WHERE id=?1",
                [&p.id],
                |r| Ok(r.get::<_, i64>(0)? as u64),
            )
            .optional()?;
        let data = p.encode_to_vec();
        self.capacity(
            (data.len() as u64).saturating_sub(old.unwrap_or(0)) as usize,
            old.is_none(),
        )?;
        self.conn.execute("INSERT INTO basemap_packages(id,meta,path) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET meta=excluded.meta,path=excluded.path",params![p.id,data,path.to_string_lossy()])?;
        self.payload_bytes =
            self.payload_bytes.saturating_sub(old.unwrap_or(0)) + data.len() as u64;
        if old.is_none() {
            self.records += 1;
        }
        Ok(())
    }
    pub(crate) fn basemap_binding(&self, key: &str) -> Result<String> {
        Ok(self
            .conn
            .query_row(
                "SELECT package_id FROM basemap_refs WHERE owner=?1",
                [key],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or_default())
    }
    pub(crate) fn bind_basemap(
        &mut self,
        key: &str,
        context: &maps::BasemapContext,
        id: &str,
    ) -> Result<()> {
        let old: Option<u64> = self
            .conn
            .query_row(
                "SELECT length(context) FROM basemap_refs WHERE owner=?1",
                [key],
                |r| Ok(r.get::<_, i64>(0)? as u64),
            )
            .optional()?;
        let bytes = context.encode_to_vec();
        self.capacity(
            (bytes.len() as u64).saturating_sub(old.unwrap_or(0)) as usize,
            old.is_none(),
        )?;
        self.conn.execute("INSERT INTO basemap_refs(owner,context,package_id) VALUES(?1,?2,?3) ON CONFLICT(owner) DO UPDATE SET context=excluded.context,package_id=excluded.package_id",params![key,bytes,id])?;
        self.payload_bytes =
            self.payload_bytes.saturating_sub(old.unwrap_or(0)) + bytes.len() as u64;
        if old.is_none() {
            self.records += 1;
        }
        Ok(())
    }
    pub(crate) fn basemap_references(&self, id: &str) -> Result<Vec<String>> {
        let mut query = self
            .conn
            .prepare("SELECT owner,context FROM basemap_refs WHERE package_id=?1")?;
        let rows = query.query_map([id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (key, bytes) = row?;
            let context = maps::BasemapContext::decode(bytes.as_slice())?;
            let exists = if !context.job_id.is_empty() {
                self.conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM jobs WHERE id=?1)",
                    [context.job_id],
                    |r| r.get::<_, bool>(0),
                )?
            } else if !context.scenario_id.is_empty() {
                self.conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM scenarios WHERE id=?1 AND version=?2)",
                    params![context.scenario_id, context.version as i64],
                    |r| r.get(0),
                )?
            } else {
                self.conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM operations WHERE id=?1)",
                    [context.operation_id],
                    |r| r.get(0),
                )?
            };
            if exists {
                out.push(key);
            }
        }
        Ok(out)
    }
    pub(crate) fn delete_basemap(&mut self, id: &str) -> Result<PathBuf> {
        self.atomic(|s|{
            let (package,path)=s.basemap(id)?;
            if !s.basemap_references(id)?.is_empty(){return Err(AppError::conflict("basemap is referenced by a scenario or historical view"));}
            for task in s.tasks()?.iter().filter(|t|!crate::runtime::tasks::task_terminal(t)) {
                let uses=task.basemap_package_id==id || s.task_input(&task.id).ok().is_some_and(|i| matches!(i.input,Some(pb::task_input::Input::Basemap(b)) if Path::new(&b.reuse_path)==path));
                if uses {return Err(AppError::conflict("basemap is used by an active task"));}
            }
            let(bytes,count):(u64,u64)=s.conn.query_row("SELECT coalesce(sum(length(context)),0),count(*) FROM basemap_refs WHERE package_id=?1",[id],|r|Ok((r.get::<_,i64>(0)? as u64,r.get::<_,i64>(1)? as u64)))?;
            s.conn.execute("DELETE FROM basemap_refs WHERE package_id=?1",[id])?;s.conn.execute("DELETE FROM basemap_packages WHERE id=?1",[id])?;
            s.payload_bytes=s.payload_bytes.saturating_sub(package.encoded_len() as u64+bytes);s.records=s.records.saturating_sub(1+count);Ok(path)
        })
    }
    pub(crate) fn freeze_basemap_input(&mut self, id: &str, input: &pb::TaskInput) -> Result<()> {
        let previous = self.task_input(id)?;
        let old = previous.encoded_len() as u64;
        let mut input = input.clone();
        if let Some(pb::task_input::Input::Basemap(old)) = previous.input
            && let Some(pb::task_input::Input::Basemap(new)) = input.input.as_mut()
        {
            for reference in old.waiting_selections {
                if !new
                    .waiting_selections
                    .iter()
                    .any(|r| r.key == reference.key)
                {
                    new.waiting_selections.push(reference);
                }
            }
        }
        let bytes = input.encode_to_vec();
        self.capacity((bytes.len() as u64).saturating_sub(old) as usize, false)?;
        // The fingerprint retains the submitted request identity, not the source
        // snapshot resolved later by the worker controller.
        self.conn.execute(
            "UPDATE generic_tasks SET input=?2 WHERE id=?1",
            params![super::tasks::task_number(id)?, bytes],
        )?;
        self.payload_bytes = self.payload_bytes.saturating_sub(old) + bytes.len() as u64;
        Ok(())
    }
}
