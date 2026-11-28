#[path = "../../../src/knowledge/chunks/batches.rs"]
pub(crate) mod batches;
#[path = "../../../src/knowledge/chunks/heading_depth.rs"]
pub(crate) mod heading_depth;
#[path = "../../../src/knowledge/chunks/serialize.rs"]
pub(crate) mod serialize;
pub(crate) use serialize::generate_chunk_turtle;
#[cfg(test)]
mod tests {
    pub(crate) fn chunk(
        id: &str,
        path: &str,
        line: u32,
        name: Option<&str>,
    ) -> crate::types::Chunk {
        crate::types::Chunk {
            id: id.into(),
            file_path: path.into(),
            chunk_type: crate::types::ChunkType::Section,
            name: name.map(str::to_owned),
            start_line: line,
            end_line: line,
            content: String::new(),
            language: "markdown".into(),
            tags: String::new(),
        }
    }
}
