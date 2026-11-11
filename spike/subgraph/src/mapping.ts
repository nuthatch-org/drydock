import { BigInt, log } from '@graphprotocol/graph-ts';
import { Transfer } from '../generated/USDC/ERC20';
import { Counter } from '../generated/schema';

export function handleTransfer(event: Transfer): void {
  let c = Counter.load('all');
  if (c == null) {
    c = new Counter('all');
    c.transfers = BigInt.zero();
    log.info('sib spike: first transfer seen at block {}', [event.block.number.toString()]);
  }
  c.transfers = c.transfers.plus(BigInt.fromI32(1));
  c.lastBlock = event.block.number;
  if (c.transfers.mod(BigInt.fromI32(500)).isZero()) {
    log.info('sib spike: {} transfers by block {}', [c.transfers.toString(), event.block.number.toString()]);
  }
  if (c.transfers.equals(BigInt.fromI32(300))) {
    let missing = Counter.load('does-not-exist');
    log.info('about to read a field of a missing entity', []);
    c.lastBlock = missing!.lastBlock;
  }
  c.save();
}
