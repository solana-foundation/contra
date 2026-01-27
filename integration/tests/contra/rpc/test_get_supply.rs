use super::test_context::ContraContext;

pub async fn run_get_supply_test(ctx: &ContraContext) {
    println!("\n=== Get Supply Test ===");

    // Get the supply information
    let supply_response = ctx.get_supply().await.unwrap();
    println!("Supply response: {:?}", supply_response);

    // Contra has no native token supply, so all values should be 0
    assert_eq!(supply_response.value.total, 0, "Total supply should be 0");

    assert_eq!(
        supply_response.value.circulating, 0,
        "Circulating supply should be 0"
    );

    assert_eq!(
        supply_response.value.non_circulating, 0,
        "Non-circulating supply should be 0"
    );

    assert!(
        supply_response.value.non_circulating_accounts.is_empty(),
        "Non-circulating accounts list should be empty"
    );

    println!("✓ All supply values are correctly set to 0");
    println!("✓ Non-circulating accounts list is empty");
    println!("\n✓ All getSupply tests passed!");
}
