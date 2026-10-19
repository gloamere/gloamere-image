# Native candidate test-configuration fix, before code

Windows all-target strict Clippy fails because create_private_directory_new is
used by production native installers, while unit-test branches explicitly use
synthetic directory creation and the portable call-site tests are Unix-only.
The exact error is dead_code in the Windows lib-test target, not a Win32 ABI or
production type error. Add a narrowly scoped allow(dead_code) on that function
only for cfg(all(windows,test)), with the reason. Do not disable production
warnings, alter callers, owner/ACL policy or namespace checks. Rerun native fmt,
all-target strict Clippy and workspace gates; record test failures separately.

Read-only ancestry evidence also shows this host's authorized workspace and
D:/Temp are not eligible for the candidate's complete root-to-leaf policy.
C:/Users/Administrator has unresolved-principal write-EA/write-attributes110;
D:/ has AuthenticatedUsers DELETE/write-EA/write-attributes10110. A newly
protected child does not repair its ancestors. No host ACL changes, path alias
or lower production checks will be introduced to make tests pass. Actual native
end-to-end root admission remains blocked pending a controlled eligible
namespace or a separately designed and authorized compatibility decision.
