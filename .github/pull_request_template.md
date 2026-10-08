## What does this PR change?
<!-- Link the issue: Closes #123 -->

## Instructions / accounts affected
<!-- e.g. buy, migrate, new field on BondingCurve -->

## Security impact
<!-- Who can call it? Can funds move? What stops misuse? Any new signer/authority? -->

## How was it tested?
<!-- Unit tests added, devnet tx signatures -->

## Checklist
- [ ] Builds with `build-wsl.sh`
- [ ] Checked math on all amounts (no unchecked `+ - * /` on lamports/tokens)
- [ ] All account constraints (`has_one`, `seeds`, `owner`, signer) verified
- [ ] No keypairs or secrets in the diff
- [ ] IDL change noted (web repo `lib/idl.json` must be updated)
- [ ] README instruction table updated if instructions changed
