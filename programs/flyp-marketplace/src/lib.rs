use anchor_lang::prelude::*;
use anchor_spl::{
    associated_token::AssociatedToken,
    token::{self, Mint, Token, TokenAccount}
};
use mpl_token_metadata::types::Creator;


declare_id!("BWMAGH4P6JzUrP5xsyGsX2LXQXkFnHWMwNg8PpYfNsRK");

#[program]
pub mod flyp_marketplace {
    use super::*;

    // Constants
    const FEE_DENOMINATOR: u64 = 10000; // For handling basis points (100% = 10000)
    const MARKETPLACE_FEE_SHARE: u64 = 9000; // 90% of the fee goes to the marketplace
    const SECOND_BIDDER_FEE_SHARE: u64 = 1000; // 10% of the fee goes to the second highest bidder

    // Create a new listing
    pub fn create_listing(
        ctx: Context<CreateListing>,
        price: u64,
        quantity: u64,
        expiry: i64,
    ) -> Result<()> {
        let listing = &mut ctx.accounts.listing;
        let clock = Clock::get()?;

        listing.seller = ctx.accounts.seller.key();
        listing.nft_mint = ctx.accounts.nft_mint.key();
        listing.price = price;
        listing.quantity = quantity;
        listing.created_at = clock.unix_timestamp;
        listing.expiry = expiry;

        // Transfer NFT to PDA
        let cpi_accounts = token::Transfer {
            from: ctx.accounts.seller_nft_account.to_account_info(),
            to: ctx.accounts.vault_nft_account.to_account_info(),
            authority: ctx.accounts.seller.to_account_info(),
        };
        let cpi_program = ctx.accounts.token_program.to_account_info();
        let cpi_ctx = CpiContext::new(cpi_program, cpi_accounts);
        token::transfer(cpi_ctx, quantity)?;

        emit!(ListingCreated {
            listing_id: listing.key(),
            seller: ctx.accounts.seller.key(),
            nft_mint: ctx.accounts.nft_mint.key(),
            price,
            quantity,
            expiry,
        });

        Ok(())
    }

    // Cancel an existing listing
    pub fn cancel_listing(ctx: Context<CancelListing>) -> Result<()> {
        let listing = &mut ctx.accounts.listing;

        // Transfer NFT back to seller
        let seeds = &[
            b"vault".as_ref(),
            listing.nft_mint.as_ref(),
            &[ctx.bumps.vault_nft_account],
        ];
        let signer = &[&seeds[..]];

        let cpi_accounts = token::Transfer {
            from: ctx.accounts.vault_nft_account.to_account_info(),
            to: ctx.accounts.seller_nft_account.to_account_info(),
            authority: ctx.accounts.vault_nft_account.to_account_info(),
        };
        let cpi_program = ctx.accounts.token_program.to_account_info();
        let cpi_ctx = CpiContext::new_with_signer(cpi_program, cpi_accounts, signer);
        token::transfer(cpi_ctx, listing.quantity)?;

        emit!(ListingCancelled {
            listing_id: listing.key(),
            seller: ctx.accounts.seller.key(),
            nft_mint: listing.nft_mint,
        });

        Ok(())
    }

    // Execute a sale
    pub fn execute_sale(ctx: Context<ExecuteSale>, second_highest_bid: u64) -> Result<()> {
        let listing = &ctx.accounts.listing;
        let metadata = &ctx.accounts.metadata;

        // Calculate royalties
        let (creator_payments, remaining_payment) = calculate_creator_payments(
            listing.price,
            &metadata.data.creators,
        )?;

        // Calculate platform fee and distribute it
        let (marketplace_fee, second_bidder_fee, seller_payment) = calculate_and_distribute_fee(
            remaining_payment,
            second_highest_bid,
        )?;

        // Transfer payments
        transfer_payments(
            ctx,
            seller_payment,
            &creator_payments,
            marketplace_fee,
            second_bidder_fee,
        )?;

        // Transfer NFT from vault to buyer
        let seeds = &[
            b"vault".as_ref(),
            listing.nft_mint.as_ref(),
            &[ctx.bumps.vault_nft_account],
        ];
        let signer = &[&seeds[..]];

        let cpi_accounts = token::Transfer {
            from: ctx.accounts.vault_nft_account.to_account_info(),
            to: ctx.accounts.buyer_nft_account.to_account_info(),
            authority: ctx.accounts.vault_nft_account.to_account_info(),
        };
        let cpi_program = ctx.accounts.token_program.to_account_info();
        let cpi_ctx = CpiContext::new_with_signer(cpi_program, cpi_accounts, signer);
        token::transfer(cpi_ctx, 1)?;

        // Update or close the listing
        if ctx.accounts.listing.quantity == 1 {
            // Close the listing account
            let dest_account_info = ctx.accounts.seller.to_account_info();
            let close_account_info = ctx.accounts.listing.to_account_info();
            let dest_starting_lamports = dest_account_info.lamports();
            **dest_account_info.lamports.borrow_mut() = dest_starting_lamports
                .checked_add(close_account_info.lamports())
                .unwrap();
            **close_account_info.lamports.borrow_mut() = 0;
        } else {
            ctx.accounts.listing.quantity -= 1;
        }

        emit!(SaleExecuted {
            listing_id: listing.key(),
            buyer: ctx.accounts.buyer.key(),
            seller: listing.seller,
            nft_mint: listing.nft_mint,
            price: listing.price,
        });

        Ok(())
    } 

    // Place a bid on an NFT
    pub fn place_bid(ctx: Context<PlaceBid>, price: u64, expiry: i64) -> Result<()> {
        let bid = &mut ctx.accounts.bid;
        let clock = Clock::get()?;

        bid.bidder = ctx.accounts.bidder.key();
        bid.nft_mint = ctx.accounts.nft_mint.key();
        bid.price = price;
        bid.created_at = clock.unix_timestamp;
        bid.expiry = expiry;

        // Transfer bid amount to escrow
        let cpi_accounts = token::Transfer {
            from: ctx.accounts.bidder_payment_account.to_account_info(),
            to: ctx.accounts.escrow_payment_account.to_account_info(),
            authority: ctx.accounts.bidder.to_account_info(),
        };
        let cpi_program = ctx.accounts.token_program.to_account_info();
        let cpi_ctx = CpiContext::new(cpi_program, cpi_accounts);
        token::transfer(cpi_ctx, price)?;

        emit!(BidPlaced {
            bid_id: bid.key(),
            bidder: ctx.accounts.bidder.key(),
            nft_mint: ctx.accounts.nft_mint.key(),
            price,
            expiry,
        });

        Ok(())
    }

    // Cancel an existing bid
    pub fn cancel_bid(ctx: Context<CancelBid>) -> Result<()> {
        let bid = &ctx.accounts.bid;

        // Transfer bid amount back to bidder
        let seeds = &[
            b"escrow".as_ref(),
            bid.nft_mint.as_ref(),
            bid.bidder.as_ref(),
            &[ctx.bumps.escrow_payment_account],
        ];
        let signer = &[&seeds[..]];

        let cpi_accounts = token::Transfer {
            from: ctx.accounts.escrow_payment_account.to_account_info(),
            to: ctx.accounts.bidder_payment_account.to_account_info(),
            authority: ctx.accounts.escrow_payment_account.to_account_info(),
        };
        let cpi_program = ctx.accounts.token_program.to_account_info();
        let cpi_ctx = CpiContext::new_with_signer(cpi_program, cpi_accounts, signer);
        token::transfer(cpi_ctx, bid.price)?;

        emit!(BidCancelled {
            bid_id: bid.key(),
            bidder: bid.bidder,
            nft_mint: bid.nft_mint,
        });

        Ok(())
    }

    // Accept a bid
    pub fn accept_bid(ctx: Context<AcceptBid>, second_highest_bid: u64) -> Result<()> {
        let bid = &ctx.accounts.bid;
        let metadata = &ctx.accounts.metadata;

        // Calculate royalties
        let (creator_payments, remaining_payment) = calculate_creator_payments(
            bid.price,
            &metadata.data.creators,
        )?;

        // Calculate platform fee and distribute it
        let (marketplace_fee, second_bidder_fee, seller_payment) = calculate_and_distribute_fee(
            remaining_payment,
            second_highest_bid,
        )?;

        // Transfer payments
        transfer_payments(
            ctx,
            seller_payment,
            &creator_payments,
            marketplace_fee,
            second_bidder_fee,
        )?;

        // Transfer NFT to bidder
        let cpi_accounts = token::Transfer {
            from: ctx.accounts.seller_nft_account.to_account_info(),
            to: ctx.accounts.bidder_nft_account.to_account_info(),
            authority: ctx.accounts.seller.to_account_info(),
        };
        let cpi_program = ctx.accounts.token_program.to_account_info();
        let cpi_ctx = CpiContext::new(cpi_program, cpi_accounts);
        token::transfer(cpi_ctx, 1)?;

        emit!(BidAccepted {
            bid_id: bid.key(),
            seller: ctx.accounts.seller.key(),
            bidder: bid.bidder,
            nft_mint: bid.nft_mint,
            price: bid.price,
        });

        Ok(())
    }

    // Helper Functions

    pub fn calculate_creator_payments(
        ctx: Context<ExecuteSale>,
        price: u64,
        creators: &Option<Vec<Creator>>,
    ) -> Result<(Vec<(Pubkey, u64)>, u64)> {
        let mut creator_payments = Vec::new();
        let mut remaining_payment = price;

        if let Some(creators) = creators {
            for creator in creators {
                if creator.verified {
                    let creator_fee = (price as u128)
                        .checked_mul(creator.share as u128)
                        .unwrap()
                        .checked_div(100)
                        .unwrap() as u64;
                    creator_payments.push((creator.address, creator_fee));
                    remaining_payment = remaining_payment.checked_sub(creator_fee).unwrap();
                }
            }
        }

        Ok((creator_payments, remaining_payment))
    }

    pub fn calculate_and_distribute_fee(
        ctx: Context<ExecuteSale>,
        amount: u64,
        second_highest_bid: u64,
    ) -> Result<(u64, u64, u64)> {
        let platform_fee_bps = 250; // 2.5%
        let total_fee = (amount as u128)
            .checked_mul(platform_fee_bps as u128)
            .unwrap()
            .checked_div(FEE_DENOMINATOR as u128)
            .unwrap() as u64;

        let marketplace_fee = (total_fee as u128)
            .checked_mul(MARKETPLACE_FEE_SHARE as u128)
            .unwrap()
            .checked_div(FEE_DENOMINATOR as u128)
            .unwrap() as u64;

        let second_bidder_fee = (total_fee as u128)
            .checked_mul(SECOND_BIDDER_FEE_SHARE as u128)
            .unwrap()
            .checked_div(FEE_DENOMINATOR as u128)
            .unwrap() as u64;

        let adjusted_second_bidder_fee = std::cmp::min(second_bidder_fee, second_highest_bid);
        let adjusted_marketplace_fee = marketplace_fee + (second_bidder_fee - adjusted_second_bidder_fee);

        let seller_payment = amount.checked_sub(total_fee).unwrap();

        Ok((adjusted_marketplace_fee, adjusted_second_bidder_fee, seller_payment))
    }

    pub fn transfer_payments(
        ctx: Context<ExecuteSale>,
        seller_payment: u64,
        creator_payments: &[(Pubkey, u64)],
        marketplace_fee: u64,
        second_bidder_fee: u64,
    ) -> Result<()> {
        // Transfer to seller
        if seller_payment > 0 {
            let cpi_accounts = token::Transfer {
                from: ctx.accounts.buyer_payment_account.to_account_info(),
                to: ctx.accounts.seller_payment_account.to_account_info(),
                authority: ctx.accounts.buyer.to_account_info(),
            };
            let cpi_program = ctx.accounts.token_program.to_account_info();
            let cpi_ctx = CpiContext::new(cpi_program, cpi_accounts);
            token::transfer(cpi_ctx, seller_payment)?;
        }

        // Transfer to creators
        for (creator, amount) in creator_payments {
            if *amount > 0 {
                let creator_account = next_account_info(ctx.remaining_accounts.iter())?;
                let cpi_accounts = token::Transfer {
                    from: ctx.accounts.buyer_payment_account.to_account_info(),
                    to: creator_account.to_account_info(),
                    authority: ctx.accounts.buyer.to_account_info(),
                };
                let cpi_program = ctx.accounts.token_program.to_account_info();
                let cpi_ctx = CpiContext::new(cpi_program, cpi_accounts);
                token::transfer(cpi_ctx, *amount)?;
            }
        }

        // Transfer marketplace fee
        if marketplace_fee > 0 {
            let cpi_accounts = token::Transfer {
                from: ctx.accounts.buyer_payment_account.to_account_info(),
                to: ctx.accounts.marketplace_fee_account.to_account_info(),
                authority: ctx.accounts.buyer.to_account_info(),
            };
            let cpi_program = ctx.accounts.token_program.to_account_info();
        let cpi_ctx = CpiContext::new(cpi_program, cpi_accounts);
        token::transfer(cpi_ctx, marketplace_fee)?;
    }

    // Transfer fee to second highest bidder
    if second_bidder_fee > 0 {
        let cpi_accounts = token::Transfer {
            from: ctx.accounts.buyer_payment_account.to_account_info(),
            to: ctx.accounts.second_bidder_account.to_account_info(),
            authority: ctx.accounts.buyer.to_account_info(),
        };
        let cpi_program = ctx.accounts.token_program.to_account_info();
        let cpi_ctx = CpiContext::new(cpi_program, cpi_accounts);
        token::transfer(cpi_ctx, second_bidder_fee)?;
    }

    Ok(())
}

// Account structures

#[derive(Accounts)]
pub struct CreateListing<'info> {
    #[account(mut)]
    pub seller: Signer<'info>,
    pub nft_mint: Account<'info, Mint>,
    #[account(
        init,
        payer = seller,
        space = 8 + 32 + 32 + 8 + 8 + 8 + 8,
        seeds = [b"listing", seller.key().as_ref(), nft_mint.key().as_ref()],
        bump
    )]
    pub listing: Account<'info, Listing>,
    #[account(
        mut,
        associated_token::mint = nft_mint,
        associated_token::authority = seller
    )]
    pub seller_nft_account: Account<'info, TokenAccount>,
    #[account(
        init_if_needed,
        payer = seller,
        associated_token::mint = nft_mint,
        associated_token::authority = vault_nft_account
    )]
    pub vault_nft_account: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct CancelListing<'info> {
    #[account(mut)]
    pub seller: Signer<'info>,
    #[account(
        mut,
        close = seller,
        seeds = [b"listing", seller.key().as_ref(), listing.nft_mint.as_ref()],
        bump,
        has_one = seller
    )]
    pub listing: Account<'info, Listing>,
    #[account(
        mut,
        associated_token::mint = listing.nft_mint,
        associated_token::authority = seller
    )]
    pub seller_nft_account: Account<'info, TokenAccount>,
    #[account(
        mut,
        associated_token::mint = listing.nft_mint,
        associated_token::authority = vault_nft_account
    )]
    pub vault_nft_account: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct ExecuteSale<'info> {
    #[account(mut)]
    pub buyer: Signer<'info>,
    /// CHECK: We're reading data from this account
    #[account(mut)]
    pub seller: AccountInfo<'info>,
    #[account(
        mut,
        seeds = [b"listing", seller.key().as_ref(), listing.nft_mint.as_ref()],
        bump,
        has_one = seller
    )]
    pub listing: Account<'info, Listing>,
    pub nft_mint: Account<'info, Mint>,
    #[account(
        mut,
        associated_token::mint = listing.nft_mint,
        associated_token::authority = vault_nft_account
    )]
    pub vault_nft_account: Account<'info, TokenAccount>,
    #[account(
        init_if_needed,
        payer = buyer,
        associated_token::mint = listing.nft_mint,
        associated_token::authority = buyer
    )]
    pub buyer_nft_account: Account<'info, TokenAccount>,
    #[account(mut)]
    pub buyer_payment_account: Account<'info, TokenAccount>,
    #[account(mut)]
    pub seller_payment_account: Account<'info, TokenAccount>,
    /// CHECK: We're reading data from this account
    #[account(mut)]
    pub marketplace_fee_account: AccountInfo<'info>,
    /// CHECK: We're reading data from this account
    #[account(mut)]
    pub second_bidder_account: Account<'info, TokenAccount>,
    /// CHECK: We're reading data from this account
    pub metadata: AccountInfo<'info>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct PlaceBid<'info> {
    #[account(mut)]
    pub bidder: Signer<'info>,
    pub nft_mint: Account<'info, Mint>,
    #[account(
        init,
        payer = bidder,
        space = 8 + 32 + 32 + 8 + 8 + 8,
        seeds = [b"bid", bidder.key().as_ref(), nft_mint.key().as_ref()],
        bump
    )]
    pub bid: Account<'info, Bid>,
    #[account(mut)]
    pub bidder_payment_account: Account<'info, TokenAccount>,
    #[account(
        init_if_needed,
        payer = bidder,
        associated_token::mint = nft_mint,
        associated_token::authority = escrow_payment_account
    )]
    pub escrow_payment_account: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct CancelBid<'info> {
    #[account(mut)]
    pub bidder: Signer<'info>,
    #[account(
        mut,
        close = bidder,
        seeds = [b"bid", bidder.key().as_ref(), bid.nft_mint.as_ref()],
        bump,
        has_one = bidder
    )]
    pub bid: Account<'info, Bid>,
    #[account(mut)]
    pub bidder_payment_account: Account<'info, TokenAccount>,
    #[account(
        mut,
        associated_token::mint = bid.nft_mint,
        associated_token::authority = escrow_payment_account
    )]
    pub escrow_payment_account: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct AcceptBid<'info> {
    #[account(mut)]
    pub seller: Signer<'info>,
    #[account(
        mut,
        close = seller,
        seeds = [b"bid", bid.bidder.as_ref(), bid.nft_mint.as_ref()],
        bump
    )]
    pub bid: Account<'info, Bid>,
    pub nft_mint: Account<'info, Mint>,
    #[account(
        mut,
        associated_token::mint = nft_mint,
        associated_token::authority = seller
    )]
    pub seller_nft_account: Account<'info, TokenAccount>,
    #[account(
        init_if_needed,
        payer = seller,
        associated_token::mint = nft_mint,
        associated_token::authority = bid.bidder
    )]
    pub bidder_nft_account: Account<'info, TokenAccount>,
    #[account(mut)]
    pub escrow_payment_account: Account<'info, TokenAccount>,
    #[account(mut)]
    pub seller_payment_account: Account<'info, TokenAccount>,
    /// CHECK: We're reading data from this account
    #[account(mut)]
    pub marketplace_fee_account: AccountInfo<'info>,
    /// CHECK: We're reading data from this account
    #[account(mut)]
    pub second_bidder_account: Account<'info, TokenAccount>,
    /// CHECK: We're reading data from this account
    pub metadata: AccountInfo<'info>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

// Data structures

#[account]
pub struct Listing {
    pub seller: Pubkey,
    pub nft_mint: Pubkey,
    pub price: u64,
    pub quantity: u64,
    pub created_at: i64,
    pub expiry: i64,
}

#[account]
pub struct Bid {
    pub bidder: Pubkey,
    pub nft_mint: Pubkey,
    pub price: u64,
    pub created_at: i64,
    pub expiry: i64,
}

// Event structures

#[event]
pub struct ListingCreated {
    pub listing_id: Pubkey,
    pub seller: Pubkey,
    pub nft_mint: Pubkey,
    pub price: u64,
    pub quantity: u64,
    pub expiry: i64,
}

#[event]
pub struct ListingCancelled {
    pub listing_id: Pubkey,
    pub seller: Pubkey,
    pub nft_mint: Pubkey,
}

#[event]
pub struct SaleExecuted {
    pub listing_id: Pubkey,
    pub buyer: Pubkey,
    pub seller: Pubkey,
    pub nft_mint: Pubkey,
    pub price: u64,
}

#[event]
pub struct BidPlaced {
    pub bid_id: Pubkey,
    pub bidder: Pubkey,
    pub nft_mint: Pubkey,
    pub price: u64,
    pub expiry: i64,
}

#[event]
pub struct BidCancelled {
    pub bid_id: Pubkey,
    pub bidder: Pubkey,
    pub nft_mint: Pubkey,
}

#[event]
pub struct BidAccepted {
    pub bid_id: Pubkey,
    pub seller: Pubkey,
    pub bidder: Pubkey,
    pub nft_mint: Pubkey,
    pub price: u64,
}
}