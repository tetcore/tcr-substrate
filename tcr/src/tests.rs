use super::*;

use sp_core::H256;
use sp_runtime::{Perbill, traits::{BlakeTwo256, IdentityLookup, OnFinalize}, testing::Header};
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
	pub const ExistentialDeposit: u64 = 1;
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
	pub const MinDeposit: u64 = 100;
	pub const ApplyStageLen: u64 = 10;
	pub const CommitStageLen: u64 = 10;
}
impl Trait for Test {
	type Event = ();
	type ListingId = u32;
	type Currency = balances::Module<Self>;

	type MinDeposit = MinDeposit;
	type ApplyStageLen = ApplyStageLen;
	type CommitStageLen = CommitStageLen;
}
type Tcr = Module<Test>;
type System = system::Module<Test>;
type Balances = balances::Module<Test>;

// Builds the genesis config store and sets mock values.
fn new_test_ext() -> sp_io::TestExternalities {
	let mut t = system::GenesisConfig::default()
		.build_storage::<Test>()
		.unwrap();
	let _ = balances::GenesisConfig::<Test>{
		balances: vec![
			(1, 1000_000),
			(2, 1000_000),
			(3, 1000_000),
			(4, 1000_000),
		],
		vesting: vec![],
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
		// Make the proposal
		assert_ok!(Tcr::propose(
			Origin::signed(1),
			1,
			100
		));

		// Ensure the proper balance has been reserved
		assert_eq!(Balances::free_balance(1), 999_900);
		assert_eq!(Balances::reserved_balance(1), 100);
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
			Tcr::challenge(Origin::signed(1), 1, 100),
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
		assert_ok!(Tcr::challenge(Origin::signed(2), 1, 101));
	});
}

#[test]
fn promotion_works() {
	new_test_ext().execute_with(|| {

			assert_ok!(Tcr::propose(Origin::signed(1), 1, 101));
			System::set_block_number(11);
			Tcr::on_finalize(11);
			assert!(Tcr::listings(1).in_registry);
	});
}

#[test]
fn aye_vote_works_correctly() {
	new_test_ext().execute_with(|| {
		assert_ok!(Tcr::propose(Origin::signed(1), 1, 100));
		assert_ok!(Tcr::challenge(Origin::signed(2), 1, 300));
		assert_ok!(Tcr::vote(Origin::signed(1), 1, true, 50));

		// Ensure the challenges struct has been updated properly
		assert_eq!(Tcr::challenges(0).total_aye, 100 + 50);
		assert_eq!(Tcr::challenges(0).total_nay, 300);

		// Ensure the proper balances have been reserved
		assert_eq!(Balances::reserved_balance(1), 100 + 50);
		assert_eq!(Balances::reserved_balance(2), 300);
	});
}

#[test]
fn nay_vote_works_correctly() {
	new_test_ext().execute_with(|| {
		assert_ok!(Tcr::propose(Origin::signed(1), 1, 100));
		assert_ok!(Tcr::challenge(Origin::signed(2), 1, 300));
		assert_ok!(Tcr::vote(Origin::signed(3), 1, false, 50));

		// Ensure challenges struct update properly
		assert_eq!(Tcr::challenges(0).total_aye, 100);
		assert_eq!(Tcr::challenges(0).total_nay, 300 + 50);

		// Ensure balances reserved properly
		assert_eq!(Balances::reserved_balance(1), 100);
		assert_eq!(Balances::reserved_balance(2), 300);
		assert_eq!(Balances::reserved_balance(3), 50);
});
}

#[test]
fn successfully_challenged_proposals_are_removed() {
	new_test_ext().execute_with(|| {
		assert_ok!(Tcr::propose(Origin::signed(1), 1, 100));
		assert_ok!(Tcr::challenge(Origin::signed(2), 1, 300));

		Tcr::on_finalize(11);

		assert!(!Tcr::registry_contains(1))
	});
}

#[test]
fn successfully_challenged_listings_are_removed() {
	new_test_ext().execute_with(|| {

		// Propose
		assert_ok!(Tcr::propose(Origin::signed(1), 1, 100));

		// Promote
		System::set_block_number(11);
		Tcr::on_finalize(11);

		// Challenge
		System::set_block_number(12);
		assert_ok!(Tcr::challenge(Origin::signed(2), 1, 300));

		// Run on_finalize
		Tcr::on_finalize(22);

		assert!(!Tcr::registry_contains(1))
	});
}

#[test]
fn unsuccessfully_challenged_listings_are_kept() {
	new_test_ext().execute_with(|| {
		// Propose
		assert_ok!(Tcr::propose(Origin::signed(1), 1, 100));

		// Promote
		System::set_block_number(11);
		Tcr::on_finalize(11);

		// Challenge
		System::set_block_number(12);
		assert_ok!(Tcr::challenge(Origin::signed(2), 1, 300));

		// Aye vote saves listing
		assert_ok!(Tcr::vote(Origin::signed(3), 1, true, 400));

		// Run on_finalize
		Tcr::on_finalize(22);

		// Ensure listing is still in the registry
		assert!(Tcr::registry_contains(1))
	});
}
