import { createHash, randomBytes } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import {
  Connection,
  Keypair,
  PublicKey,
  SystemProgram,
  Transaction,
  TransactionInstruction,
  sendAndConfirmTransaction,
} from "@solana/web3.js";
import {
  ASSOCIATED_TOKEN_PROGRAM_ID,
  MINT_SIZE,
  TOKEN_2022_PROGRAM_ID,
  createAssociatedTokenAccountIdempotentInstruction,
  createInitializeMint2Instruction,
  createMintToInstruction,
  createTransferCheckedInstruction,
  getAccount,
  getAssociatedTokenAddressSync,
  getMint,
} from "@solana/spl-token";

const RPC = process.env.SOLANA_RPC_URL ?? "https://api.devnet.solana.com";
const PROGRAM_ID = new PublicKey(process.env.KABO_PROGRAM_ID);
const VRF_PROGRAM_ID = new PublicKey("VRFzZoJdhFWL8rkvu87LpKM3RbcVezpMEc6X5GVDr7y");
const STAKE = 2_000_000_000n;
const DECIMALS = 6;

function keypair(path) {
  return Keypair.fromSecretKey(Uint8Array.from(JSON.parse(readFileSync(path, "utf8"))));
}

function disc(name) {
  return createHash("sha256").update(`global:${name}`).digest().subarray(0, 8);
}

function u64(value) {
  const out = Buffer.alloc(8);
  out.writeBigUInt64LE(BigInt(value));
  return out;
}

async function send(connection, payer, instructions, extraSigners = []) {
  const tx = new Transaction().add(...instructions);
  return sendAndConfirmTransaction(connection, tx, [payer, ...extraSigners], {
    commitment: "confirmed",
    preflightCommitment: "confirmed",
  });
}

const connection = new Connection(RPC, "confirmed");
const payer = keypair("devnet-payer.json");
const mint = keypair("devnet-mint.json");

const mintRent = await connection.getMinimumBalanceForRentExemption(MINT_SIZE);
const payerAta = getAssociatedTokenAddressSync(
  mint.publicKey,
  payer.publicKey,
  false,
  TOKEN_2022_PROGRAM_ID,
  ASSOCIATED_TOKEN_PROGRAM_ID,
);

const [game] = PublicKey.findProgramAddressSync(
  [Buffer.from("game"), mint.publicKey.toBuffer()],
  PROGRAM_ID,
);
const [bank] = PublicKey.findProgramAddressSync(
  [Buffer.from("bank"), game.toBuffer()],
  PROGRAM_ID,
);
const vault = getAssociatedTokenAddressSync(
  mint.publicKey,
  bank,
  true,
  TOKEN_2022_PROGRAM_ID,
  ASSOCIATED_TOKEN_PROGRAM_ID,
);

const createMintSig = await send(
  connection,
  payer,
  [
    SystemProgram.createAccount({
      fromPubkey: payer.publicKey,
      newAccountPubkey: mint.publicKey,
      lamports: mintRent,
      space: MINT_SIZE,
      programId: TOKEN_2022_PROGRAM_ID,
    }),
    createInitializeMint2Instruction(
      mint.publicKey,
      DECIMALS,
      payer.publicKey,
      null,
      TOKEN_2022_PROGRAM_ID,
    ),
    createAssociatedTokenAccountIdempotentInstruction(
      payer.publicKey,
      payerAta,
      payer.publicKey,
      mint.publicKey,
      TOKEN_2022_PROGRAM_ID,
      ASSOCIATED_TOKEN_PROGRAM_ID,
    ),
    createMintToInstruction(
      mint.publicKey,
      payerAta,
      payer.publicKey,
      100_000_000_000_000n,
      [],
      TOKEN_2022_PROGRAM_ID,
    ),
  ],
  [mint],
);

const initializeIx = new TransactionInstruction({
  programId: PROGRAM_ID,
  keys: [
    { pubkey: payer.publicKey, isSigner: true, isWritable: true },
    { pubkey: mint.publicKey, isSigner: false, isWritable: false },
    { pubkey: game, isSigner: false, isWritable: true },
    { pubkey: bank, isSigner: false, isWritable: false },
    { pubkey: vault, isSigner: false, isWritable: true },
    { pubkey: TOKEN_2022_PROGRAM_ID, isSigner: false, isWritable: false },
    { pubkey: ASSOCIATED_TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
    { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
  ],
  data: Buffer.concat([disc("initialize"), u64(STAKE)]),
});
const initializeSig = await send(connection, payer, [initializeIx]);

const bankroll = 50_000_000_000n;
const fundSig = await send(connection, payer, [
  createTransferCheckedInstruction(
    payerAta,
    mint.publicKey,
    vault,
    payer.publicKey,
    bankroll,
    DECIMALS,
    [],
    TOKEN_2022_PROGRAM_ID,
  ),
]);

const nonce = BigInt(Date.now());
const nonceBytes = u64(nonce);
const seed = randomBytes(32);
const [spin] = PublicKey.findProgramAddressSync(
  [Buffer.from("spin"), game.toBuffer(), payer.publicKey.toBuffer(), nonceBytes],
  PROGRAM_ID,
);
const [vrfConfig] = PublicKey.findProgramAddressSync(
  [Buffer.from("orao-vrf-network-configuration")],
  VRF_PROGRAM_ID,
);
const configInfo = await connection.getAccountInfo(vrfConfig, "confirmed");
if (!configInfo || configInfo.data.length < 72) throw new Error("ORAO Devnet config is unavailable");
const vrfTreasury = new PublicKey(configInfo.data.subarray(40, 72));
const [randomness] = PublicKey.findProgramAddressSync(
  [Buffer.from("orao-vrf-randomness-request"), seed],
  VRF_PROGRAM_ID,
);

const playerBefore = (await getAccount(connection, payerAta, "confirmed", TOKEN_2022_PROGRAM_ID)).amount;
const vaultBefore = (await getAccount(connection, vault, "confirmed", TOKEN_2022_PROGRAM_ID)).amount;
const supplyBefore = (await getMint(connection, mint.publicKey, "confirmed", TOKEN_2022_PROGRAM_ID)).supply;

const startIx = new TransactionInstruction({
  programId: PROGRAM_ID,
  keys: [
    { pubkey: payer.publicKey, isSigner: true, isWritable: true },
    { pubkey: game, isSigner: false, isWritable: true },
    { pubkey: mint.publicKey, isSigner: false, isWritable: false },
    { pubkey: payerAta, isSigner: false, isWritable: true },
    { pubkey: bank, isSigner: false, isWritable: false },
    { pubkey: vault, isSigner: false, isWritable: true },
    { pubkey: spin, isSigner: false, isWritable: true },
    { pubkey: randomness, isSigner: false, isWritable: true },
    { pubkey: vrfConfig, isSigner: false, isWritable: true },
    { pubkey: vrfTreasury, isSigner: false, isWritable: true },
    { pubkey: VRF_PROGRAM_ID, isSigner: false, isWritable: false },
    { pubkey: TOKEN_2022_PROGRAM_ID, isSigner: false, isWritable: false },
    { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
  ],
  data: Buffer.concat([disc("start_spin"), nonceBytes, seed]),
});
const startSig = await send(connection, payer, [startIx]);

const settleIx = new TransactionInstruction({
  programId: PROGRAM_ID,
  keys: [
    { pubkey: payer.publicKey, isSigner: true, isWritable: true },
    { pubkey: game, isSigner: false, isWritable: true },
    { pubkey: spin, isSigner: false, isWritable: true },
    { pubkey: payer.publicKey, isSigner: false, isWritable: false },
    { pubkey: mint.publicKey, isSigner: false, isWritable: true },
    { pubkey: payerAta, isSigner: false, isWritable: true },
    { pubkey: bank, isSigner: false, isWritable: false },
    { pubkey: vault, isSigner: false, isWritable: true },
    { pubkey: randomness, isSigner: false, isWritable: false },
    { pubkey: TOKEN_2022_PROGRAM_ID, isSigner: false, isWritable: false },
    { pubkey: ASSOCIATED_TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
    { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
  ],
  data: disc("settle_spin"),
});

let settleSig;
let lastError;
for (let attempt = 1; attempt <= 30; attempt += 1) {
  await new Promise((resolve) => setTimeout(resolve, 2_000));
  try {
    settleSig = await send(connection, payer, [settleIx]);
    break;
  } catch (error) {
    lastError = error;
    console.log(`Settlement attempt ${attempt} is still pending`);
  }
}
if (!settleSig) throw lastError ?? new Error("ORAO fulfillment timed out");

const spinInfo = await connection.getAccountInfo(spin, "confirmed");
if (!spinInfo || spinInfo.data.length < 195) throw new Error("Spin account missing or truncated");
const finished = spinInfo.data[168] === 1;
const slot = spinInfo.data[169];
const gross = spinInfo.data.readBigUInt64LE(170);
const paid = spinInfo.data.readBigUInt64LE(178);
const burned = spinInfo.data.readBigUInt64LE(186);
const refunded = spinInfo.data[194] === 1;
if (!finished || refunded) throw new Error("Spin did not settle normally");
if (gross === 0n) {
  if (paid !== 0n || burned !== (STAKE * 2_000n) / 10_000n) {
    throw new Error("Losing-spin burn accounting failed");
  }
} else if (gross !== paid + burned) {
  throw new Error("Winning-spin payout accounting failed");
}

const playerAfter = (await getAccount(connection, payerAta, "confirmed", TOKEN_2022_PROGRAM_ID)).amount;
const vaultAfter = (await getAccount(connection, vault, "confirmed", TOKEN_2022_PROGRAM_ID)).amount;
const supplyAfter = (await getMint(connection, mint.publicKey, "confirmed", TOKEN_2022_PROGRAM_ID)).supply;
if (supplyBefore - supplyAfter !== burned) throw new Error("Mint supply burn mismatch");
if (playerAfter !== playerBefore - STAKE + paid) throw new Error("Player token balance mismatch");
if (vaultAfter !== vaultBefore + STAKE - paid - burned) throw new Error("Vault token balance mismatch");

const receipt = {
  cluster: "devnet",
  programId: PROGRAM_ID.toBase58(),
  payer: payer.publicKey.toBase58(),
  mint: mint.publicKey.toBase58(),
  game: game.toBase58(),
  bank: bank.toBase58(),
  vault: vault.toBase58(),
  spin: spin.toBase58(),
  randomness: randomness.toBase58(),
  transactions: { createMintSig, initializeSig, fundSig, startSig, settleSig },
  result: {
    slot,
    stake: STAKE.toString(),
    gross: gross.toString(),
    paid: paid.toString(),
    burned: burned.toString(),
    playerBefore: playerBefore.toString(),
    playerAfter: playerAfter.toString(),
    vaultBefore: vaultBefore.toString(),
    vaultAfter: vaultAfter.toString(),
    supplyBefore: supplyBefore.toString(),
    supplyAfter: supplyAfter.toString(),
  },
};
writeFileSync("devnet-smoke-receipt.json", `${JSON.stringify(receipt, null, 2)}\n`);
console.log(JSON.stringify(receipt, null, 2));
