use std::time::Duration;

use poise::serenity_prelude::{self, CacheHttp, ChannelId, Http, ReactionType, RoleId, UserId};
use sqlx::PgPool;
use tracing::*;
use uuid::Uuid;
use vrsc::Amount;
use vrsc_rpc::{Client, RpcApi};

use crate::{
    commands::{misc::Notification, user_blacklisted},
    util::database::{self, store_new_address_for_user},
    wallet::check_and_get_balance,
    Context, Error,
};

/// Tip a user or a role
///
/// -------- :robot: **Tipping a user** --------
/// Tip a role by entering and selecting the user name. The selection menu will update as you type.
///
/// -------- :robot: **Tipping a role** --------
/// Tip a role by entering and selecting the role name. The role name can be any role, even the @everyone role. \
/// The amount entered in the second parameter will be split evenly among the members of the role.
#[instrument(skip(_ctx), fields(request_id = %Uuid::new_v4() ))]
#[poise::command(slash_command, category = "Tipping", subcommands("role", "user"))]
pub async fn tip(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Tip a role by entering and selecting the role name.
#[instrument(skip(ctx), fields(request_id = %Uuid::new_v4() ))]
#[poise::command(slash_command, category = "Tipping")]
async fn role(
    ctx: Context<'_>,
    #[description = "Enter and select the role you want to tip"] role: serenity_prelude::Role,
    #[description = "The amount you want to tip"]
    #[min = 0.5]
    tip_amount: f64,
) -> Result<(), Error> {
    if user_blacklisted(ctx, ctx.author().id).await? {
        return Ok(());
    }

    debug!("role: {:?}", role.id);
    let tip_amount = Amount::from_vrsc(tip_amount)?;

    if check_and_get_balance(&ctx, tip_amount).await?.is_some() {
        trace!("tipper has enough balance");
        let pool = &ctx.data().database;

        if let Some(guild) = ctx.guild() {
            debug!("guildid: {:?}", guild.id);
            let guild_members = guild.members.values();
            let role_members = guild_members
                .filter(
                    |m| m.roles.contains(&role.id) || &role.id == &RoleId(guild.id.0), // @everyone role_id (same as guild_id) does never get tips
                )
                .map(|m| m.user.id)
                .collect::<Vec<_>>();

            tip_users(
                ctx,
                // &ctx.http(),
                // &ctx.data().verus()?,
                // &pool,
                &role_members,
                // &ctx.author().id,
                // &ctx.channel_id(),
                &tip_amount,
                "role",
            )
            .await?;

            return Ok(());
        } else {
            trace!("not in a guild, send error");

            ctx.send(|reply| {
                reply.ephemeral(true).content(format!(
                    "You need to be in a Discord server to use this command."
                ))
            })
            .await?;

            return Ok(());
        }
    }

    Ok(())
}

/// Tip a user by entering and selecting the user's name.
#[instrument(skip(ctx), fields(request_id = %Uuid::new_v4() ))]
#[poise::command(slash_command, category = "Tipping")]
async fn user(
    ctx: Context<'_>,
    #[description = "Enter and select the user you want to tip"] user: serenity_prelude::User,
    #[description = "The amount you want to tip"] tip_amount: f64,
) -> Result<(), Error> {
    if user_blacklisted(ctx, ctx.author().id).await? {
        return Ok(());
    }

    let tip_amount = Amount::from_vrsc(tip_amount)?;

    debug!(
        "user {} ({}) wants to tip {} with {tip_amount}",
        ctx.author().name,
        ctx.author().id,
        user.id
    );

    // check if the tipper has enough balance
    // update both balances in 1 go

    let pool = &ctx.data().database;

    if check_and_get_balance(&ctx, tip_amount).await?.is_some() {
        trace!("tipper has enough balance");
        // we can tip!
        // what if the user we are about to tip has no balance?
        // we need to create a balance for him first. TODO: Maybe we can do that in the command itself.
        // if get_balance_for_user(pool, &user.id).await?.is_none() {
        //     trace!("balance is none, so need to create new balance for user.");
        //     let client = &ctx.data().verus()?;
        //     let address = client.get_new_address()?;
        //     store_new_address_for_user(pool, &user.id, &address).await?;
        // }

        // trace!("the tippee has a balance, we can tip now.");

        database::tip_user(pool, &ctx.author().id, &user.id, &tip_amount).await?;

        // tips are only stored one way: counterparty is the sender of the tip.
        let tip_event_id = Uuid::new_v4();
        database::store_tip_transaction(
            pool,
            &tip_event_id,
            &user.id,
            "direct",
            &tip_amount,
            ctx.author().id.0,
        )
        .await?;

        let notification: Notification =
            database::get_notification_setting(&pool, &user.id).await?;

        match notification {
            Notification::All | Notification::ChannelOnly => {
                // send a message in the same channel:
                ctx.send(|reply| {
                    reply.ephemeral(false).content(format!(
                        "<@{}> just tipped <@{}> {tip_amount}!",
                        &ctx.author().id,
                        user.id
                    ))
                })
                .await?;
            }
            Notification::DMOnly => {
                // send a non-pinging message in the channel:
                ctx.send(|reply| {
                    reply.ephemeral(false).content(format!(
                        "<@{}> just tipped `{}` {tip_amount}!",
                        &ctx.author().id,
                        user.tag()
                    ))
                })
                .await?;
                // send a notification in dm:
                user.dm(&ctx.http(), |message| {
                    message.content(format!(
                        "You just got tipped {tip_amount} from <@{}>!",
                        &ctx.author().id,
                    ))
                })
                .await?;
            }
            Notification::Off => {
                // send a non-pinging message in the channel:
                ctx.send(|reply| {
                    reply.ephemeral(false).content(format!(
                        "<@{}> just tipped `{}` {tip_amount}!",
                        &ctx.author().id,
                        user.tag()
                    ))
                })
                .await?;
            }
        }

        return Ok(());
    }

    Ok(())
}

#[derive(Debug, poise::ChoiceParameter)]
pub enum Hms {
    Hours,
    Minutes,
    Seconds,
}

/// Start a giveaway where users need to react to a message to participate
///
/// -------- :robot: **Reactdrop** --------
/// When initiating a reactdrop, find a suitable emoji in the first parameter. \
/// It can be any Emoji, as long as the emoji is in the current server.
///
/// The amount is entered in the second parameter. This amount will be split among the participants of the reactdrop when it ends.
#[instrument(skip(ctx), fields(request_id = %Uuid::new_v4() ))]
#[poise::command(slash_command, category = "Tipping")]
pub async fn reactdrop(
    ctx: Context<'_>,
    #[description = "The emoji users need to react with"] emoji: String,
    #[min = 0.1]
    #[description = "The amount you want to give away"]
    amount: f64,
    #[min = 1] time: u32,
    #[description = "The time in hours, minutes or seconds"] hms: Hms,
) -> Result<(), Error> {
    if user_blacklisted(ctx, ctx.author().id).await? {
        return Ok(());
    }

    // a reactdrop can be started for as long as a user wants it to last. Discord however limits the lifetime of a context to 15 minutes.
    // We must account for this by extracting the necessary data from `Context` and store it for later use.
    let tip_amount = Amount::from_vrsc(amount)?;

    if check_and_get_balance(&ctx, tip_amount).await?.is_some() {
        let pool = &ctx.data().database;

        debug!("emoji picked for reactdrop: {}", emoji);

        if let Ok(reaction_type) = ReactionType::try_from(emoji) {
            match &reaction_type {
                ReactionType::Custom {
                    animated: _,
                    id,
                    name: _,
                } => {
                    let emojis = ctx.guild().unwrap().emojis(ctx.http()).await?;
                    if !emojis.iter().any(|e| e.id == id.0) {
                        trace!("emoji not in guild");
                        ctx.say("This emoji is not found in this Discord server, so it can't be used. Please pick another one").await?;

                        return Ok(());
                    } else {
                        debug!("emoji in guild");
                    }
                }
                ReactionType::Unicode(unicode) => {
                    trace!("a unicode emoji was given. Check if it is really emoji.");
                    let regex = fancy_regex::Regex::new(
                        r"/((?<!\\)<:[^:]+:(\d+)>)|\p{Emoji}|\p{Extended_Pictographic}/gmu",
                    )?;

                    if regex.find(&unicode)?.is_none() {
                        ctx.say("This is not an emoji. Please pick an emoji to start a Reactdrop")
                            .await?;

                        return Ok(());
                    } else {
                        trace!("valid unicode");
                    }
                }
                ref s => {
                    unreachable!("we find ourselves in a weird state: {:?}", s);
                }
            }

            trace!("valid emoji");

            let context = ctx.serenity_context().to_owned();

            let http = context.http.clone();
            let http_2 = context.http.clone();

            let mut i: u32 = match hms {
                Hms::Hours => time * 60 * 60,
                Hms::Minutes => time * 60,
                Hms::Seconds => time,
            };
            let reply_handle = ctx.say(format!(">>> **A reactdrop of {tip_amount} was started!**\n\nReact with the {} emoji to participate\n\nTime remaining: {} {}", reaction_type.clone(), time, hms)).await?;
            let mut msg = reply_handle.into_message().await?;
            msg.react(ctx.http(), reaction_type.clone()).await?;

            let channel_id = ctx.channel_id();

            loop {
                match i {
                    mut j if i > 120 => {
                        let mut interval = tokio::time::interval(Duration::from_secs(60));

                        while j > 120 {
                            interval.tick().await;

                            msg.edit(http.clone(), |f| {
                                f.content(format!(">>> **A reactdrop of {tip_amount} was started!**\n\nReact with the {} emoji to participate\n\nTime remaining: {} hour(s), {} minute(s)",
                                &reaction_type,
                                j / 60 / 60,
                                (j / 60) % 60))
                            })
                            .await?;

                            i -= 60;
                            j -= 60;
                        }
                        interval.tick().await;
                    }
                    mut j => {
                        let mut interval = tokio::time::interval(Duration::from_secs(1));

                        while j > 0 {
                            interval.tick().await;
                            msg.edit(http.clone(), |f| {
                                f.content(format!(">>> **A reactdrop of {tip_amount} was started!**\n\nReact with the {} emoji to participate\n\nTime remaining: {} seconds", &reaction_type, j))
                            })
                            .await?;
                            trace!("time remaining: {} seconds", j);
                            i -= 1;
                            j -= 1;
                        }
                        interval.tick().await;
                        msg.edit(http.clone(), |f| {
                            f.content(format!(">>> **A reactdrop of {tip_amount} was started!**\n\nReact with the {} emoji to participate\n\nTime remaining: {} seconds", &reaction_type, j))
                        })
                        .await?;

                        break;
                    }
                };
            }

            let mut last_user = None;

            loop {
                if let Ok(users) = msg
                    .reaction_users(http_2.clone(), reaction_type.clone(), None, last_user)
                    .await
                {
                    last_user = users.last().map(|user| user.id);
                    if last_user.is_none() {
                        break;
                    }
                    debug!("users: {:#?}", &users);
                    let users = users
                        .iter()
                        .filter(|user| !user.bot)
                        .map(|u| u.id)
                        .collect::<Vec<_>>();

                    if users.len() == 0 {
                        trace!("no users to tip, abort");
                    } else {
                        trace!("tipping {} users in reactdrop", users.len());
                        tip_users(
                            ctx,
                            // &http,
                            // &ctx.data().verus()?,
                            // pool,
                            &users,
                            // &ctx.author().id,
                            // &ctx.channel_id(),
                            &tip_amount,
                            "reactdrop",
                        )
                        .await?;
                    }

                    continue;
                }
            }

            channel_id
                .delete_reaction_emoji(http, msg, reaction_type)
                .await?;
        }
    }

    Ok(())
}

async fn tip_users(
    ctx: Context<'_>,
    users: &Vec<UserId>,
    amount: &Amount,
    kind: &str,
) -> Result<(), Error> {
    // TODO optimize this query (select all that don't exist, insert them in 1 go)
    // check if all the tippees have an entry in the db
    let pool = &ctx.data().database;
    let client = ctx.data().verus()?;
    let author = &ctx.author().id;
    let http = ctx.http();

    // for user_id in users.iter() {
    //     if database::get_address_from_user(pool, user_id)
    //         .await?
    //         .is_none()
    //     {
    //         trace!("need to get new address");
    //         let address = client.get_new_address()?;
    //         store_new_address_for_user(pool, user_id, &address).await?;
    //     }
    // }

    debug!("users in tip_users: {:?}", users);

    // need to divide tipping amount over number of people in a role
    if let Some(div_tip_amount) = amount.checked_div(users.len() as u64) {
        let amount = div_tip_amount
            .checked_mul(users.len() as u64)
            .unwrap_or(*amount);
        debug!("after division every member gets {div_tip_amount}");
        debug!("members: {:#?}", &users);

        let tip_event_id = Uuid::new_v4();

        database::tip_users(pool, &author, &users, &div_tip_amount).await?;

        database::store_multiple_tip_transactions(
            pool,
            &tip_event_id,
            users,
            kind,
            &div_tip_amount,
            &author,
        )
        .await?;

        let notification_settings = database::get_notification_setting_batch(pool, &users).await?;

        for (user_id, notification) in notification_settings {
            match (user_id, notification) {
                (_, Notification::All) | (_, Notification::DMOnly) => {
                    let user = UserId(user_id as u64).to_user(http).await?;
                    user.dm(http, |message| {
                        message.content(format!(
                            "You just got tipped {div_tip_amount} from <@{}>!",
                            &author,
                        ))
                    })
                    .await?;
                }
                _ => {
                    // don't ping when ChannelOnly or Off
                }
            }
        }

        ctx.send(|message| {
            message.content(format!(
                "<@{}> just tipped {} to {} users! ({} each)",
                &author,
                amount,
                &users.len(),
                div_tip_amount
            ))
        })
        .await?;
    } else {
        ctx.send(|message| message.content("Could not send tip to role"))
            .await?;
    }

    Ok(())
}
