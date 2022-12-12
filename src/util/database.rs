use std::str::FromStr;

use crate::Error;
use poise::serenity_prelude::UserId;
use sqlx::PgPool;
use tracing::*;
use uuid::Uuid;
use vrsc::{Address, Amount};
use vrsc_rpc::bitcoin::Txid;

/// Queries the database and retrieves the balance for the user, if it exists.
/// If there is no row for this user, None will be returned.
pub async fn get_balance_for_user(pool: &PgPool, user_id: UserId) -> Result<Option<u64>, Error> {
    if let Some(row) = sqlx::query!(
        "SELECT balance FROM balance_vrsc WHERE discord_id = $1",
        user_id.0 as i64
    )
    .fetch_optional(pool)
    .await?
    {
        let balance = row.balance.unwrap(); // balance is always there since it's NOTNULL
        Ok(Some(balance as u64))
    } else {
        Ok(None)
    }
}

pub async fn store_new_address_for_user(
    pool: &PgPool,
    user_id: UserId,
    address: &Address,
) -> Result<(), Error> {
    sqlx::query!(
        "WITH inserted_row AS (
            INSERT INTO discord_users (discord_id, vrsc_address) 
            VALUES ($1, $2)
        )
        INSERT INTO balance_vrsc (discord_id)
        VALUES ($1)
        ",
        user_id.0 as i64,
        &address.to_string()
    )
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn get_address_from_user(
    pool: &PgPool,
    user_id: UserId,
) -> Result<Option<Address>, Error> {
    if let Some(row) = sqlx::query!(
        "SELECT discord_id, vrsc_address FROM discord_users WHERE discord_id = $1",
        user_id.0 as i64
    )
    .fetch_optional(pool)
    .await?
    {
        Ok(Some(Address::from_str(&row.vrsc_address)?))
    } else {
        Ok(None)
    }
}

pub async fn get_user_from_address(
    pool: &PgPool,
    address: &Address,
) -> Result<Option<UserId>, Error> {
    if let Some(row) = sqlx::query!(
        "SELECT discord_id FROM discord_users WHERE vrsc_address = $1",
        &address.to_string()
    )
    .fetch_optional(pool)
    .await?
    {
        Ok(Some(UserId(row.discord_id as u64)))
    } else {
        Ok(None)
    }
}

pub async fn transaction_processed(pool: &PgPool, txid: &Txid) -> Result<bool, Error> {
    let transaction_query = sqlx::query!(
        "SELECT * FROM transactions_vrsc WHERE transaction_id = $1 AND transaction_action = 'deposit'",
        &txid.to_string()
    )
    .fetch_optional(pool)
    .await?;

    match transaction_query {
        Some(_) => Ok(true),
        None => Ok(false),
    }
}

pub async fn increase_balance(
    pool: &PgPool,
    user_id: &UserId,
    amount: Amount,
) -> Result<(), Error> {
    debug!(
        "going to increase balance for {user_id} with {} VRSC",
        amount.as_vrsc()
    );
    let result = sqlx::query!(
        "UPDATE balance_vrsc SET balance = balance + $1 WHERE discord_id = $2",
        amount.as_sat() as i64,
        user_id.0 as i64
    )
    .execute(pool)
    .await;

    match result {
        Ok(result) => info!("increasing the balance went ok! {:?}", result),
        Err(e) => return Err(e.into()),
    }

    Ok(())
}

pub async fn decrease_balance(
    pool: &PgPool,
    user_id: &UserId,
    amount: &Amount,
    tx_fee: &Amount,
) -> Result<(), Error> {
    if let Some(to_decrease) = amount.checked_add(*tx_fee) {
        debug!(
            "going to decrease balance for {user_id} with {} VRSC",
            to_decrease.as_vrsc()
        );
        let result = sqlx::query!(
            "UPDATE balance_vrsc SET balance = balance - $1 WHERE discord_id = $2",
            to_decrease.as_sat() as i64,
            user_id.0 as i64
        )
        .execute(pool)
        .await;

        match result {
            Ok(result) => info!("decreasing the balance went ok! {:?}", result),
            Err(e) => return Err(e.into()),
        }
    } else {
        // summing the 2 balances went wrong. This is an edge case that only happens when someone is withdrawing more than 184,467,440,737.09551615 VRSC,
        // which is more than the supply of VRSC will ever be.
        unreachable!()
        // TODO: It could be that a PBaaS chain will have such a supply, in which case we need to catch the error and inform the user. But not needed right now.
    }
    Ok(())
}

pub async fn store_deposit_transaction(
    pool: &PgPool,
    uuid: &Uuid,
    user_id: &UserId,
    tx_hash: &Txid,
) -> Result<(), Error> {
    sqlx::query!(
        "INSERT INTO transactions_vrsc (uuid, discord_id, transaction_id, transaction_action) VALUES ($1, $2, $3, $4)",
        uuid.to_string(),
        user_id.0 as i64,
        tx_hash.to_string(),
        "deposit"
        )
        .execute(pool)
        .await?;

    Ok(())
}

pub async fn store_withdraw_transaction(
    pool: &PgPool,
    uuid: &Uuid,
    user_id: &UserId,
    tx_hash: &Txid,
    opid: &str,
    tx_fee: &Amount,
) -> Result<(), Error> {
    sqlx::query!(
        "INSERT INTO transactions_vrsc (uuid, discord_id, transaction_id, opid, transaction_action, fee) VALUES ($1, $2, $3, $4, $5, $6)",
        uuid.to_string(),
        user_id.0 as i64,
        tx_hash.to_string(),
        opid,
        "withdraw",
        tx_fee.as_sat() as i64
        )
        .execute(pool)
        .await?;

    Ok(())
}
