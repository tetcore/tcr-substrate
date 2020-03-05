
#![cfg_attr(not(feature = "std"), no_std)]

use codec::{Decode, Encode};
use sp_std::prelude::*;
use sp_runtime::traits::CheckedAdd;
use frame_support::{
	decl_event, decl_module, decl_storage, dispatch::DispatchResult, ensure, Parameter,
	traits::{ Currency, ReservableCurrency, Get },
};
use system::ensure_signed;

// Read TCR concepts here:
// https://www.gautamdhameja.com/token-curated-registries-explain-eli5-a5d4cce0ddbe/

#[cfg(test)]
mod tests;

// The module trait
pub trait Trait: system::Trait {
	type Event: From<Event<Self>> + Into<<Self as system::Trait>::Event>;
	type Currency: Currency<Self::AccountId> + ReservableCurrency<Self::AccountId>;
	type ListingId: Parameter + Encode + Decode + Default;
	// The TCR Parameters
	type MinDeposit: Get<BalanceOf<Self>>;
	type ApplyStageLen: Get<Self::BlockNumber>;
	type CommitStageLen: Get<Self::BlockNumber>;
}

type ChallengeId = u32;
type BalanceOf<T> = <<T as Trait>::Currency as Currency<<T as system::Trait>::AccountId>>::Balance;
type AccountIdOf<T> = <T as system::Trait>::AccountId;
type BlockNumberOf<T> = <T as system::Trait>::BlockNumber;
type ListingIdOf<T> = <T as Trait>::ListingId;

type ListingDetailOf<T> = ListingDetail<BalanceOf<T>, AccountIdOf<T>, BlockNumberOf<T>>;
type ChallengeDetailOf<T> = ChallengeDetail<<T as Trait>::ListingId, BalanceOf<T>, AccountIdOf<T>, VoteOf<T>>;
type VoteOf<T> = Vote<AccountIdOf<T>, BalanceOf<T>>;

#[cfg_attr(feature = "std", derive(Debug))]
#[derive(Encode, Decode, Default, Clone, PartialEq)]
pub struct ListingDetail<Balance, AccountId, BlockNumber> {
	deposit: Balance,
	owner: AccountId,
	application_expiry: Option<BlockNumber>,
	in_registry: bool,
	challenge_id: Option<ChallengeId>,
}

#[cfg_attr(feature = "std", derive(Debug))]
#[derive(Encode, Decode, Default, Clone, PartialEq)]
pub struct ChallengeDetail<ListingId, Balance, AccountId, Vote> {
	listing_id: ListingId,
	deposit: Balance,
	owner: AccountId,
	total_aye: Balance,
	total_nay: Balance,
	votes: Vec<Vote>,
}

#[cfg_attr(feature = "std", derive(Debug))]
#[derive(Encode, Decode, Default, Clone, PartialEq)]
pub struct Vote<AccountId, Balance> {
	voter: AccountId,
	aye_or_nay: bool, // true means: I want this item in the registry. false means: I do not want this item in the registry
	deposit: Balance,
}

decl_storage! {
	trait Store for Module<T: Trait> as Tcr {
		/// TCR parameter - minimum deposit.
		// MinDeposit get(min_deposit) config(): Option<BalanceOf<T>>;
		//
		// /// TCR parameter - apply stage length - deadline for challenging before a listing gets accepted.
		// ApplyStageLen get(apply_stage_len) config(): Option<T::BlockNumber>;
		//
		// /// TCR parameter - commit stage length - deadline for voting before a challenge gets resolved.
		// CommitStageLen get(commit_stage_len) config(): Option<T::BlockNumber>;


		/// All listings and applicants known to the TCR. Inclusion in this map is NOT the same as listing in the registry,
		/// because this map also includes new applicants (some of which are challenged)
		Listings get(listings): map hasher(blake2_256) T::ListingId => ListingDetailOf<T>;

		/// All currently open challenges
		Challenges get(challenges): map ChallengeId => ChallengeDetailOf<T>;

		/// The first unused challenge Id. Will become the Id of the next challenge when it is open.
		NextChallengeId get(next_challenge_id): ChallengeId;

		/// Mapping from the blocknumber when a challenge expires to its challenge Id. This is used to
		/// automatically resolve challenges in `on_finalize`. This storage item could be omitted if
		/// settling challenges were a mnaully triggered process.
		ChallengeExpiry get(challenge_expiry): map BlockNumberOf<T> => Vec<ChallengeId>;
	}
}

// Events
decl_event!(
	pub enum Event<T>
		where AccountId = <T as system::Trait>::AccountId,
		Balance = BalanceOf<T>,
		ListingId = ListingIdOf<T>,
	{
		/// A user has proposed a new listing
		Proposed(AccountId, ListingId, Balance),

		/// A user has challenged a listing. The challenged listing may be already listed,
		/// or an applicant
		Challenged(AccountId, ListingId, ChallengeId, Balance),

		/// A user cast a vote in an already-existing challenge
		Voted(AccountId, ChallengeId, bool, Balance),

		/// A challenge has been resolved and the challenged listing included or excluded from the registry.
		/// This does not guarantee that the status of the challenged listing in the registry has changed.
		/// For example, a previously-listed item may have passed the challenge, or a new applicant may have
		/// failed the challenge.
		Resolved(ListingId, bool),

		/// A new, previously un-registered listing has been added to the Registry
		Accepted(ListingId),

		/// A previously-registered listing, or a proposla has been rejected.
		Rejected(ListingId),
	}
);

decl_module! {
	pub struct Module<T: Trait> for enum Call where origin: T::Origin {

		// const MinDeposit: BalanceOf<T> = T::MinDeposit::get();
		// const ApplyStangeLen: T::BlockNumber = T::ApplyStageLen::get();
		// const CommitStageLen: T::BlockNumber = T::CommitStageLen::get();

		// Initialize events for this module.
		fn deposit_event() = default;

		/// Propose a listing on the registry.
		fn propose(origin, proposed_listing: ListingIdOf<T>, deposit: BalanceOf<T>) -> DispatchResult {
			let sender = ensure_signed(origin)?;

			ensure!(deposit >= T::MinDeposit::get(), "deposit should be more than min_deposit");

			ensure!(!<Listings<T>>::exists(&proposed_listing), "Listing already exists");

			// Set application expiry for the listing.
			// Generating a future timestamp by adding the apply stage length.
			let now = <system::Module<T>>::block_number();
			let app_exp = now.checked_add(&T::ApplyStageLen::get()).ok_or("Overflow when setting application expiry.")?;

			// Create a new listing instance and store it.
			let listing = ListingDetailOf::<T> {
				deposit,
				owner: sender.clone(),
				application_expiry: Some(app_exp),
				in_registry: false,
				challenge_id: None,
			};

			// Reserve the application deposit.
			T::Currency::reserve(&sender, deposit)
				.map_err(|_| "Proposer can't afford deposit")?;

			// Add the listing to the map
			<Listings<T>>::insert(&proposed_listing, listing);

			// Raise the event.
			Self::deposit_event(RawEvent::Proposed(sender, proposed_listing, deposit));
			Ok(())
		}

		/// Promote an unchallenged and matured application to the registry
		fn promote_application(origin, listing_id: ListingIdOf<T>) -> DispatchResult {
			let _ = ensure_signed(origin);

			// Ensure the listing exists
			ensure!(<Listings<T>>::exists(&listing_id), "No such application to promote");

			// Grab the listing from strage
			let mut listing = <Listings<T>>::get(&listing_id);

			// Ensure the listing is an unchallenged application ready for promotion
			ensure!(listing.challenge_id == None, "Cannot promote a challenged listing.");
			ensure!(listing.application_expiry != None, "Cannot promote a listing that is not an application.");

			let expiry = listing.application_expiry.expect("Just checked that expiry is some; qed");
			let now = <system::Module<T>>::block_number();
			ensure!(expiry <= now, "Too early to promote this application.");

			// Mutate the listing, and make the promotion
			listing.application_expiry = None;
			<Listings<T>>::insert(&listing_id, listing);

			// Raise the event
			Self::deposit_event(RawEvent::Accepted(listing_id));
			Ok(())
		}

		/// Challenge a listing
		fn challenge(origin, listing_id: ListingIdOf<T>, deposit: BalanceOf<T>) -> DispatchResult {
			let challenger = ensure_signed(origin)?;

			// Ensure the listing exists and grab it
			ensure!(<Listings<T>>::exists(&listing_id), "Listing not found.");
			let mut listing = Self::listings(&listing_id);

			ensure!(listing.challenge_id == None, "Listing is already challenged.");
			ensure!(listing.owner != challenger, "You cannot challenge your own listing.");
			ensure!(deposit >= listing.deposit, "Not enough deposit to challenge.");

			// Calculate end of voting
			let now = <system::Module<T>>::block_number();
			let voting_exp = now.checked_add(&T::CommitStageLen::get()).ok_or("Overflow when setting voting expiry.")?;

			// If the listing was an unchallenged application, that is now irrelevant
			listing.application_expiry = None;

			// Update the listing's corresponding challenge Id
			let challenge_id = NextChallengeId::get();
			listing.challenge_id = Some(challenge_id);

			let challenge = ChallengeDetailOf::<T> {
				listing_id: listing_id.clone(),
				deposit: deposit.clone(),
				owner: challenger.clone(),
				total_aye: listing.deposit,
				total_nay: deposit,
				votes: Vec::new(),
			};

			// Reserve the deposit for challenge.
			T::Currency::reserve(&challenger, deposit)
				.map_err(|_| "Challenger can't afford the deposit")?;

			// Update storage items
			NextChallengeId::put(challenge_id + 1);
			<Challenges<T>>::insert(challenge_id, challenge);
			<Listings<T>>::insert(&listing_id, listing);
			<ChallengeExpiry<T>>::append_or_insert(voting_exp, &vec![challenge_id]);

			// Raise the event.
			Self::deposit_event(RawEvent::Challenged(challenger, listing_id, challenge_id, deposit));
			Ok(())
		}

		/// Registers a vote for a particular challenge.
		fn vote(origin, listing_id: ListingIdOf<T>, vote_bool: bool, deposit: BalanceOf<T>) -> DispatchResult {
			let voter = ensure_signed(origin)?;

			// Check listing exists and is challenged.
			ensure!(<Listings<T>>::exists(&listing_id), "Listing does not exist.");
			let challenge_id = <Listings<T>>::get(&listing_id).challenge_id;
			ensure!(challenge_id != None, "Listing is not challenged.");
			let challenge_id = challenge_id.expect("Just checked to ensure it's not None; qed");

			// Deduct the deposit for vote.
			T::Currency::reserve(&voter, deposit)
				.map_err(|_| "Voter can't afford the deposit")?;

			// Update votes in challenge storage
			let vote = VoteOf::<T> {
				voter: voter.clone(),
				aye_or_nay: vote_bool,
				deposit: deposit,
			};
			let mut challenge = <Challenges<T>>::get(challenge_id);
			challenge.votes.push(vote);
			if vote_bool {
				challenge.total_aye += deposit;
			}
			else {
				challenge.total_nay += deposit;
			}

			// Update storage.
			<Challenges<T>>::insert(challenge_id, challenge);

			// Raise the event.
			Self::deposit_event(RawEvent::Voted(voter, challenge_id, vote_bool, deposit));
			Ok(())
		}

		// Sets the TCR parameters.
		// Currently only min deposit, apply stage length and commit stage length are supported.
		// fn set_config(
		// 	origin,
		// 	min_deposit: BalanceOf<T>,
		// 	apply_stage_len: T::BlockNumber,
		// 	commit_stage_len: T::BlockNumber
		// ) -> DispatchResult {
		//
		// 	ensure_root(origin)?;
		//
		// 	<MinDeposit<T>>::put(min_deposit);
		// 	<ApplyStageLen<T>>::put(apply_stage_len);
		// 	<CommitStageLen<T>>::put(commit_stage_len);
		//
		// 	Ok(())
		// }

		/// Resolves challenges that expire during this block
		fn on_finalize(now: T::BlockNumber) {

			// If no challenges are ending, return early
			if !<ChallengeExpiry<T>>::exists(now) {
				return ();
			}

			// Take the expiring challenges from the runtime storage
			let challenge_ids = <ChallengeExpiry<T>>::get(now);
			<ChallengeExpiry<T>>::remove(now);

			for challenge_id in challenge_ids.iter() {
				// Grab the challnege and the listing
				let challenge = <Challenges<T>>::take(&challenge_id);
				let mut listing = <Listings<T>>::get(&challenge.listing_id);
				let previously_registered = listing.in_registry;

				// Count the vote
				let listing_is_good = challenge.total_aye > challenge.total_nay;
				Self::deposit_event(RawEvent::Resolved(challenge.listing_id.clone(), listing_is_good));
				if listing_is_good {
					// slash challenger's deposit
					T::Currency::unreserve(&challenge.owner, challenge.deposit);
					T::Currency::slash(&challenge.owner, challenge.deposit);

					// add item to registry
					listing.in_registry = true;
					Listings::<T>::insert(&challenge.listing_id, listing);

					// Emit event for newly-registered listings
					if !previously_registered {
						Self::deposit_event(RawEvent::Accepted(challenge.listing_id));
					}

				} else {
					// slash owner's deposit
					T::Currency::unreserve(&listing.owner, listing.deposit);
					T::Currency::slash(&listing.owner, listing.deposit);

					// release challenger's deposit
					T::Currency::unreserve(&challenge.owner, challenge.deposit);

					// remove item from registry
					listing.in_registry = false;
					Listings::<T>::remove(&challenge.listing_id);

					// Emit event for newly de-registered listings
					if previously_registered {
						Self::deposit_event(RawEvent::Rejected(challenge.listing_id));
					}
				}

				// Loop through votes releasing or slashing as necessary
				for vote in challenge.votes.iter() {
					T::Currency::unreserve(&vote.voter, vote.deposit);
					if vote.aye_or_nay != listing_is_good {
						T::Currency::slash(&vote.voter, vote.deposit);
					}
				}
			}
		}
	}
}

impl<T: Trait> Module<T> {
	pub fn registry_contains(l: ListingIdOf<T>) -> bool {
		Listings::<T>::exists(l)
	}
}
