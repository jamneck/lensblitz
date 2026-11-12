# Homebrew formula for LensBlitz.
#
# The repository is private, so Homebrew needs git access to it (an SSH key or a credential
# helper). Once a release is tagged, add the `url` and `sha256` of its source tarball:
#
#   url "https://github.com/jamneck/lensblitz/archive/refs/tags/v0.1.0.tar.gz"
#   sha256 "<output of: shasum -a 256 v0.1.0.tar.gz>"
#
# Until then the formula is HEAD-only: `brew install --HEAD lensblitz`.
class Lensblitz < Formula
  desc "Extremely fast, read-only Markdown viewer for macOS"
  homepage "https://github.com/jamneck/lensblitz"
  head "https://github.com/jamneck/lensblitz.git", branch: "main"

  depends_on "rust" => :build
  depends_on :macos

  def install
    system "cargo", "install", *std_cargo_args
    # Wrap the binary in LensBlitz.app so Finder can use it to open Markdown files.
    system "scripts/bundle.sh", "--binary", bin/"lensblitz", "--out", prefix
  end

  def caveats
    <<~EOS
      The command-line viewer is ready:
        lensblitz README.md

      To open Markdown files from Finder, copy the app into your Applications folder:
        cp -R "#{opt_prefix}/LensBlitz.app" /Applications/

      Then make it the default for Markdown: select any .md file in Finder, press ⌘I,
      choose LensBlitz under "Open with", and click "Change All…".
    EOS
  end

  test do
    assert_match(/^lensblitz \d+\.\d+/, shell_output("#{bin}/lensblitz --version"))
    assert_path_exists prefix/"LensBlitz.app/Contents/Info.plist"
  end
end
