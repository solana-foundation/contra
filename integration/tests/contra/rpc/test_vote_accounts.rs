use crate::rpc::ContraContext;

pub async fn run_vote_accounts_test(ctx: &ContraContext) {
    println!("\n=== Vote Accounts Test ===");

    // Get vote accounts
    let vote_accounts = ctx.get_vote_accounts().await.unwrap();
    println!("Vote accounts: {:?}", vote_accounts);

    // Contra has no voting/staking mechanism, so both arrays should be empty
    assert!(
        vote_accounts.current.is_empty(),
        "Current vote accounts should be empty"
    );

    assert!(
        vote_accounts.delinquent.is_empty(),
        "Delinquent vote accounts should be empty"
    );
}
