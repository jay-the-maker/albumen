# DCO remediation record

History in this repository is not rewritten, and there are no force pushes. When a commit on `main` carries a wrong sign-off, the correction is recorded here in a follow-up commit.

## bae3003 (#191)

The squash commit `bae30035eeb5e238faf7984fc9a3c383ad01e116` ("Report-only: disk_model and image next to Windows medians; compare only within one disk class", PR #191) carries a mistyped trailer:

    Signed-off-by: Jay Strickland <jay@users.noreply.github.com>

That address was a typing error by the agent that merged the PR, and it is not an identity of the contributor. These are the correct trailers for the commit:

    Signed-off-by: Cursor Agent <cursoragent@cursor.com>
    Co-authored-by: Jay Strickland <jay-the-maker@users.noreply.github.com>

The content of #191 was submitted by Cursor Agent, which signs the DCO for it under the trailer above. This commit carries the same trailers.
