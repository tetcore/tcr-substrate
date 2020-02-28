
#![cfg_attr(not(feature = "std"), no_std)]

use codec::{Decode, Encode};
use sp_std::prelude::*;
use sp_runtime::traits::{CheckedAdd, CheckedDiv, CheckedMul, Hash, SimpleArithmetic, Member};
use frame_support::{
	decl_event, decl_module, decl_storage, dispatch::{DispatchResult, DispatchError}, ensure, Parameter,
	traits::{ Currency, ReservableCurrency },
};
use system::{ensure_signed, ensure_root};

// Read TCR concepts here:
// https://www.gautamdhameja.com/token-curated-registries-explain-eli5-a5d4cce0ddbe/

// The module trait
pub trait Trait: system::Trait {
	type Event: From<Event<Self>> + Into<<Self as system::Trait>::Event>;
	type Currency: Currency<Self::AccountId> + ReservableCurrency<Self::AccountId>;
	type ListingId: Parameter + Encode + Decode + Default;
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
		MinDeposit get(min_deposit) config(): Option<BalanceOf<T>>;

		/// TCR parameter - apply stage length - deadline for challenging before a listing gets accepted.
		ApplyStageLen get(apply_stage_len) config(): Option<T::BlockNumber>;

		/// TCR parameter - commit stage length - deadline for voting before a challenge gets resolved.
		CommitStageLen get(commit_stage_len) config(): Option<T::BlockNumber>;


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
		ListingId = ListingIdOf<T>
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

		///
		Rejected(ListingId),
		// When a vote reward is claimed for a challenge.
		Claimed(AccountId, u32),

		//TODO MAybe the last few events should be Added, Removed, Rejected, Defended
	}
);

decl_module! {
	pub struct Module<T: Trait> for enum Call where origin: T::Origin {
		// Initialize events for this module.
		fn deposit_event() = default;

		// Propose a listing on the registry.
		fn propose(origin, proposed_listing: ListingIdOf<T>, deposit: BalanceOf<T>) -> DispatchResult {
			let sender = ensure_signed(origin)?;

			let min_deposit = Self::min_deposit().ok_or("Min deposit not set")?;
			ensure!(deposit >= min_deposit, "deposit should be more than min_deposit");

			ensure!(!<Listings<T>>::exists(&proposed_listing), "Listing already exists");

			// Set application expiry for the listing.
			// Generating a future timestamp by adding the apply stage length.
			let now = <system::Module<T>>::block_number();
			let apply_stage_len = Self::apply_stage_len().ok_or("Apply stage length not set.")?;
			let app_exp = now.checked_add(&apply_stage_len).ok_or("Overflow when setting application expiry.")?;

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
		fn promote_aplication_to_registry(origin, listing_id: ListingIdOf<T>) -> DispatchResult {
			let _ = ensure_signed(origin);

			// Ensure the listing exists
			ensure!(<Listings<T>>::exists(&listing_id), "No such application to promote");

			// Grab the listing from strage
			let mut listing = <Listings<T>>::get(&listing_id);

			// Ensure the listing is an unchallenged application ready for promotion
			ensure!(listing.challenge_id == None, "Cannot promote a challenged listing.");
			match listing.application_expiry {
				None => return Err(DispatchError::Other("Cannot promote a listing that is not an application.")),
				Some(x) if x < <system::Module<T>>::block_number() =>
					return Err(DispatchError::Other("Too early to promote this application.")),
				_ => {
					// Mutate the listing, and make the promotion
					listing.application_expiry = None;
					<Listings<T>>::insert(&listing_id, listing);

					// Raise the event
					Self::deposit_event(RawEvent::Accepted(listing_id));
					Ok(())
				}
			}
		}

		// Challenge a listing.
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
			let commit_stage_len = Self::commit_stage_len().ok_or("Commit stage length not set.")?;
			let voting_exp = now.checked_add(&commit_stage_len).ok_or("Overflow when setting voting expiry.")?;

			// If the listing was an unchallenged application, that is now irrelevant
			listing.application_expiry = None;

			// Update the listing's corresponding challenge Id
			let challenge_id = NextChallengeId::get();
			listing.challenge_id = Some(challenge_id);

			let challenge = ChallengeDetailOf::<T> {
				listing_id: listing_id.clone(),
				deposit: deposit.clone(),
				owner: challenger.clone(),
				total_aye: 0.into(),
				total_nay: 0.into(),
				votes: Vec::new(),
			};

			// Reserve the deposit for challenge.
			T::Currency::reserve(&challenger, deposit)
				.map_err(|_| "Challenger can't afford the deposit")?;

			// Update storage items
			NextChallengeId::put(challenge_id + 1);
			<Challenges<T>>::insert(challenge_id, challenge);
			<Listings<T>>::insert(&listing_id, listing);
			<ChallengeExpiry<T>>::append(voting_exp, &vec![challenge_id]);

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
			let challenge_id = challenge_id.unwrap();//.expect("Just checked to ensure it's not None; qed");

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

			// Update storage.
			<Challenges<T>>::insert(challenge_id, challenge);

			// Raise the event.
			Self::deposit_event(RawEvent::Voted(voter, challenge_id, vote_bool, deposit));
			Ok(())
		}

		// Sets the TCR parameters.
		// Currently only min deposit, apply stage length and commit stage length are supported.
		fn set_config(
			origin,
			min_deposit: BalanceOf<T>,
			apply_stage_len: T::BlockNumber,
			commit_stage_len: T::BlockNumber
		) -> DispatchResult {

			ensure_root(origin)?;

			<MinDeposit<T>>::put(min_deposit);
			<ApplyStageLen<T>>::put(apply_stage_len);
			<CommitStageLen<T>>::put(commit_stage_len);

			Ok(())
		}

		// Resolves challenges that expire during this block
		fn on_finalize(now: T::BlockNumber) {

			// If no challenges are ending, return early
			if !<ChallengeExpiry<T>>::exists(now) {
				return ();
			}

			let challenge_ids = <ChallengeExpiry<T>>::get(now);
			<ChallengeExpiry<T>>::remove(now);

			for challenge_id in challenge_ids.iter() {
				// Grab the challnege and the listing
				let challenge = <Challenges<T>>::get(&challenge_id);
				<Challenges<T>>::remove(&challenge_id);
				let mut listing = <Listings<T>>::get(challenge.listing_id);

				// Count the vote
				let passed = challenge.total_aye > challenge.total_nay;
				if passed {
					// slash challenger's deposit
					// add item to registry
				} else {
					// slash owner's deposit
					// release challenger's deposit
					// remove item fro mregistry
				}

				// Loop through votes releasing or slashing as necessary
				for vote in challenge.votes.iter() {
					//TODO
				}
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	use sp_core::H256;
	use sp_runtime::{Perbill, traits::{BlakeTwo256, IdentityLookup}, testing::Header};
	use frame_support::{impl_outer_origin, assert_ok, assert_noop, parameter_types, weights::Weight};

	impl_outer_origin! {
		pub enum Origin for Test {}
	}

	// For testing the module, we construct most of a mock runtime. This means
	// first constructing a configuration type (`Test`) which `impl`s each of the
	// configuration traits of modules we want to use.
	#[derive(Clone, Eq, PartialEq)]
	pub struct Test;
	parameter_types! {
		pub const BlockHashCount: u64 = 250;
		pub const MaximumBlockWeight: Weight = 1024;
		pub const MaximumBlockLength: u32 = 2 * 1024;
		pub const AvailableBlockRatio: Perbill = Perbill::one();
	}
	impl system::Trait for Test {
		type Origin = Origin;
		type Index = u64;
		type Call = ();
		type BlockNumber = u64;
		type Hash = H256;
		type Hashing = BlakeTwo256;
		type AccountId = u64;
		type Lookup = IdentityLookup<Self::AccountId>;
		type Header = Header;
		type Event = ();
		type BlockHashCount = BlockHashCount;
		type MaximumBlockWeight = MaximumBlockWeight;
		type AvailableBlockRatio = AvailableBlockRatio;
		type MaximumBlockLength = MaximumBlockLength;
		type Version = ();
		type ModuleToIndex = ();
	}
	parameter_types! {
		pub const ExistentialDeposit: u64 = 500;
		pub const TransferFee: u64 = 0;
		pub const CreationFee: u64 = 0;
	}
	impl balances::Trait for Test {
		type Balance = u64;
		type OnFreeBalanceZero = ();
		type DustRemoval = ();
		type Event = ();
		type ExistentialDeposit = ExistentialDeposit;
		type TransferFee = TransferFee;
		type CreationFee = CreationFee;
		type OnNewAccount = ();
		type TransferPayment = ();
	}
	parameter_types! {
		pub const MinimumPeriod: u64 = 5;
	}
	impl timestamp::Trait for Test {
		type Moment = u64;
		type OnTimestampSet = ();
		type MinimumPeriod = MinimumPeriod;
	}
	impl Trait for Test {
		type Event = ();
		type ListingId = u32;
		type Currency = balances::Module<Self>;
	}
	type Tcr = Module<Test>;

	// Builds the genesis config store and sets mock values.
	fn new_test_ext() -> sp_io::TestExternalities {
		let mut t = system::GenesisConfig::default()
			.build_storage::<Test>()
			.unwrap();
		GenesisConfig::<Test> {
			min_deposit: 100,
			apply_stage_len: 10,
			commit_stage_len: 10,
		}.assimilate_storage(&mut t).unwrap();
		t.into()
	}

	#[test]
	fn should_fail_low_deposit() {
		new_test_ext().execute_with(|| {
			assert_noop!(
				Tcr::propose(Origin::signed(1), 1, 99),
				"deposit should be more than min_deposit"
			);
		});
	}

	#[test]
	fn should_pass_propose() {
		new_test_ext().execute_with(|| {
			assert_ok!(Tcr::propose(
				Origin::signed(1),
				1,
				101
			));
		});
	}

	#[test]
	fn should_fail_challenge_same_owner() {
		new_test_ext().execute_with(|| {
			assert_ok!(Tcr::propose(
				Origin::signed(1),
				1,
				101
			));
			assert_noop!(
				Tcr::challenge(Origin::signed(1), 0, 101),
				"You cannot challenge your own listing."
			);
		});
	}

	#[test]
	fn should_pass_challenge() {
		new_test_ext().execute_with(|| {
			assert_ok!(Tcr::propose(
				Origin::signed(1),
				1,
				101
			));
			assert_ok!(Tcr::challenge(Origin::signed(2), 0, 101));
		});
	}
}
