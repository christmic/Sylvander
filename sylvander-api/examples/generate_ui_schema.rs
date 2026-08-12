fn main() -> Result<(), Box<dyn std::error::Error>> {
    let schema = sylvander_api::schema::ui_protocol_schema();
    println!("{}", serde_json::to_string_pretty(&schema)?);
    Ok(())
}
