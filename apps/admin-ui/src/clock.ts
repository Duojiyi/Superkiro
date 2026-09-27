// The server's clock, as the pricing screens read it. The console learns the server's time from
// whole-second Date headers, so it can trail the server by up to a second; the second is counted
// as begun, so what the server has just stamped "now" (a face value's repricing, a first price)
// reads as in force rather than as scheduled.
import {adminApi} from './api';

export const pricingNow = () => Math.floor(adminApi.serverNowMs / 1000) + 1;
