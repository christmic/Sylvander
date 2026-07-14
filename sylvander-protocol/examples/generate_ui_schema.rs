fn main() -> Result<(), Box<dyn std::error::Error>> {
    let schema = sylvander_protocol::schema::ui_protocol_schema();
    println!("{}", serde_json::to_string_pretty(&schema)?);
    Ok(())
}
