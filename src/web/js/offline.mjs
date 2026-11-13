// The one error that means "the computer did not answer", shared by the page
// and the sync loop so both can tell a lost connection from a refusal.

export class Offline extends Error {}
