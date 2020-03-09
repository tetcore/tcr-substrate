
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
	type ListingId: Parameter + Encode + Decode + Default + Copy;
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

		/// All listings and applicants known to the TCR. Inclusion in this map is NOT the same as listing in the registry,
		/// because this map also includes new applicants (some of which are challenged)
		Listings get(listings): map hasher(blake2_256) T::ListingId => ListingDetailOf<T>;

		/// All currently open challenges
		Challenges get(challenges): map ChallengeId => ChallengeDetailOf<T>;

		/// The first unused challenge Id. Will become the Id of the next challenge when it is open.
		NextChallengeId get(next_challenge_id): ChallengeId;

		/// Mapping from the blocknumber when a listing may need a status update to the Listing Id
		/// tht may need the update. This is used to automatically resolve challenges and promote
		/// unchallenged listings in `on_finalize`. Not all entries in this map will actually need
		/// an update. For example, an application that has been challenged will not actually be
		/// updated at its original application expiry.
		ListingsToUpdate get(challenge_expiry): map BlockNumberOf<T> => Vec<T::ListingId>;
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

			// Add the listing to the maps
			<Listings<T>>::insert(&proposed_listing, listing);
			<ListingsToUpdate<T>>::append_or_insert(app_exp, &vec![proposed_listing]);

			// Raise the event.
			Self::deposit_event(RawEvent::Proposed(sender, proposed_listing, deposit));
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
			<ListingsToUpdate<T>>::append_or_insert(voting_exp, &vec![listing_id]);

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

		/// Resolves challenges that expire during this block
		fn on_finalize(now: T::BlockNumber) {

			// If nothing needs updated, return early
			if !<ListingsToUpdate<T>>::exists(now) {
				return ();
			}

			// Take the listings in question from the runtime storage
			let listing_ids = <ListingsToUpdate<T>>::get(now);
			<ListingsToUpdate<T>>::remove(now);

			for listing_id in listing_ids.iter() {
				// Grab the listing
				let mut listing = <Listings<T>>::get(&listing_id);

				// See whether we're here because of application expiry
				if listing.application_expiry == Some(now) {
					// See if the application has gone unchallenged
					if listing.challenge_id == None {
						Self::promote_application(*listing_id, &mut listing);
					}
					else {
						// Some listings will have been marked for update at this block because their
						// application would have expired now, but have been challenged in the meantime.
						listing.application_expiry = None;
					}
				}
				else {
					// Make sure a challenge is epiring
					match listing.challenge_id {
						Some(_) => {
							Self::settle_challenge(*listing_id, &mut listing);
						}
						None => { /* Nothing to do. Happens when challenge resolved before application expiry */}
					}
				}
			}

			// Return
			()
		}
	}
}

impl<T: Trait> Module<T> {
	pub fn registry_contains(l: ListingIdOf<T>) -> bool {
		if Listings::<T>::exists(l) {
			Listings::<T>::get(l).in_registry
		}
		else {
			false
		}
	}

	fn promote_application(listing_id: ListingIdOf<T>, listing: &mut ListingDetailOf<T>) {

			// Mutate the listing, and make the promotion
			listing.application_expiry = None;
			listing.in_registry = true;
			<Listings<T>>::insert(&listing_id, listing);

			// Raise the event
			Self::deposit_event(RawEvent::Accepted(listing_id));
	}

	fn settle_challenge(listing_id: ListingIdOf<T>, listing: &mut ListingDetailOf<T>) {

		// Note whether the listing was previously registered, for event emission
		// (if not, it is a challenged application)
		let previously_registered = listing.in_registry;

		// Lookup challenge and count the vote
		let challenge_id = Listings::<T>::get(listing_id).challenge_id.expect("Confirmed a challenge existed before calling; qed");
		let challenge = Challenges::<T>::get(challenge_id);
		let listing_is_good = challenge.total_aye > challenge.total_nay;

		Self::deposit_event(RawEvent::Resolved(challenge.listing_id.clone(), listing_is_good));
		if listing_is_good {
			// slash challenger's deposit
			T::Currency::unreserve(&challenge.owner, challenge.deposit);
			T::Currency::slash(&challenge.owner, challenge.deposit);

			// add item to registry
			listing.in_registry = true;
			listing.challenge_id = None;
			Listings::<T>::insert(listing_id, listing);

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
