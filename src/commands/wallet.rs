use qrcode::{render::unicode, QrCode};
use tracing::*;
use uuid::Uuid;
use vrsc::{Address, Amount};
use vrsc_rpc::RpcApi;

use crate::{util::database, Context, Error};

#[instrument(skip(ctx), fields(request_id = %Uuid::new_v4() ))]
#[poise::command(slash_command, category = "Wallet")]
pub async fn balance(ctx: Context<'_>) -> Result<(), Error> {
    debug!(
        "user {} ({}) demands a balance",
        ctx.author().name,
        ctx.author().id
    );
    let pool = &ctx.data().database;

    if let Some(balance) = database::get_balance_for_user(&pool, ctx.author().id).await? {
        let balance_amount = Amount::from_sat(balance);

        trace!(
            "there is a balance for this user, return it: {:?}",
            &balance_amount
        );

        ctx.send(|reply| {
            reply
                .ephemeral(true)
                .content(format!("Your balance is: {}", balance_amount))
        })
        .await?;
    } else {
        trace!("there is no balance for this user");

        ctx.send(|reply| reply.ephemeral(true).content("Your balance is: 0"))
            .await?;
    }

    Ok(())
}

/// Deposit funds to the tipbot wallet
#[instrument(skip(ctx), fields(request_id = %Uuid::new_v4() ))]
#[poise::command(slash_command, category = "Wallet")]
pub async fn deposit(ctx: Context<'_>) -> Result<(), Error> {
    debug!(
        "user {} ({}) demands a deposit address",
        ctx.author().name,
        ctx.author().id
    );
    let pool = &ctx.data().database;
    let client = &ctx.data().verus;

    if let Some(address) = database::get_address_from_user(&pool, ctx.author().id).await? {
        debug!("address already stored, return it");
        send_address_message(ctx, address).await?;
    } else {
        // for some reason, an error is returned from the client: HttpResponseTooShort (got 0, expected 12)
        // todo for now redo the get_new_address RPC until we get an address.
        let address;
        loop {
            match client.get_new_address() {
                Ok(new_address) => {
                    address = new_address;
                    break;
                }
                Err(_e) => {
                    warn!("didn't get address, trying again");
                    continue;
                }
            }
        }
        // simultaneously add row to both `discord_users` and `balance_vrsc` with an initial balance of 0.
        if database::store_new_address_for_user(&pool, ctx.author().id, &address)
            .await
            .is_ok()
        {
            send_address_message(ctx, address).await?;
        }
    }

    Ok(())
}

async fn send_address_message(ctx: Context<'_>, address: Address) -> Result<(), Error> {
    ctx.send(|reply| {
        let qr = QrCode::new(&address.to_string()).unwrap();
        let image_str = qr
            .render::<unicode::Dense1x2>()
            .module_dimensions(1, 1)
            .build();

        reply.ephemeral(true).embed(|embed| {
            embed.title(format!("Deposit address: {}", &address)).field(
                "code",
                format!("```{image_str}```"),
                false,
            )
        })
    })
    .await?;

    Ok(())
}

#[allow(dead_code)]
struct DiscordUserDBData {
    discord_id: i64,
    vrsc_address: String,
}