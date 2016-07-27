require 'pathname'
require 'securerandom'
require 'time'

# Setup:
# apt-get install net-tools

class Platform
    attr_accessor :hab_bin;
    attr_accessor :hab_key_cache;
    attr_accessor :hab_org;
    attr_accessor :hab_origin;
    attr_accessor :hab_pkg_path;
    attr_accessor :hab_ring;
    attr_accessor :hab_service_group;
    attr_accessor :hab_studio_root;
    attr_accessor :hab_sup_bin;
    attr_accessor :hab_user;

    attr_accessor :log_dir;
    attr_accessor :log_name;

    def unique_name()
        SecureRandom.uuid
    end

    def env_vars()
        #TODO: share these between Inspec and Rspec?
        return %w(HAB_AUTH_TOKEN
                  HAB_CACHE_KEY_PATH
                  HAB_DEPOT_URL
                  HAB_ORG
                  HAB_ORIGIN
                  HAB_ORIGIN_KEYS
                  HAB_RING
                  HAB_RING_KEY
                  HAB_STUDIOS_HOME
                  HAB_STUDIO_ROOT
                  HAB_USER)
    end
end

class LinuxPlatform < Platform
    def initialize
        @hab_bin="/src/components/hab/target/debug/hab"
        @hab_key_cache=Dir.mktmpdir("hab_test")
        @hab_org = "org_#{unique_name()}"
        @hab_origin = "origin_#{unique_name()}"
        @hab_pkg_path = "/hab/pkgs"
        @hab_ring = "ring_#{unique_name()}"
        # todo
        @hab_service_group = "service_group_#{unique_name()}"
        @hab_studio_root=Dir.mktmpdir("hab_test_studio")
        @hab_sup_bin="/src/components/sup/target/debug/hab-sup"
        @hab_user = "user_#{unique_name()}"

        @log_name = "hab_test-#{Time.now.utc.iso8601.gsub(/\:/, '-')}.log"
        @log_dir = "./"
    end

    def cmd(cmdline)
        fullcmdline = "#{@hab_bin} #{cmdline} | tee -a #{log_file_name()} 2>&1"
        # record the command we'll be running in the log file
        `echo #{fullcmdline} >> #{log_file_name()}`
        puts "Running: #{fullcmdline}"
        pid = spawn(fullcmdline)
        Process.wait pid
        return $?
    end

    def log_file_name()
        File.join(@log_dir, @log_name)
    end

    def mk_temp_dir()
        # TODO: remove temp directory before creating
        # TODO: keep track of temp files and remove them upon success?
        dir = Dir.mktmpdir("hab_test")
        puts "Temp dir = #{dir}"
        return dir
    end

end

class WindowsPlatform
    def initialize
        raise "Windows platform not implemented"
    end
end

ctx = LinuxPlatform.new()
puts "---------------------------------------------------"
puts "Test params:"
ctx.instance_variables.sort.each do |k|
    puts "#{k[1..-1]} = #{ctx.instance_variable_get(k)}"
end
puts "Logging command output to #{ctx.log_file_name()}"
puts "---------------------------------------------------"

describe "Habitat CLI" do

    before(:all) do
        # ensure we are starting with an empty set of env vars
        # this _could_ be a test, but since we also set env vars in the
        # before() block, it makes a chicken/egg issue.
        #ctx.env_vars.each do |e|
        #    raise "#{e} is currently set, please clear the value and try again" \
        #        unless ENV[e].nil?
        #end

        ENV['HAB_CACHE_KEY_PATH']=ctx.hab_key_cache

        ctx.cmd("origin key generate #{ctx.hab_origin}")
        ctx.cmd("user key generate #{ctx.hab_user}")
        ctx.cmd("ring key generate #{ctx.hab_ring}")
        # remove the studio if it already exists
        ctx.cmd("studio rm #{ctx.hab_origin}")
        puts "Creating new studio, this may take a few minutes"
        ctx.cmd("studio -k #{ctx.hab_origin} new")
    end

    after(:all) do
        puts "Clearing test environment"
        ENV.delete('HAB_CACHE_KEY_PATH')
        #FileUtils.remove_entry(Hab.hab_key_cache)
        # TODO: kill the studio only if all tests pass?
        #ctx.cmd("studio rm")
    end

    # these are in RSpec instead of Inspec because we
    # keep some platform independent paths inside the ctx.
    # Perhaps this could be shared in the future.
    context "core cli binaries" do
        it "hab command should be compiled" do
            expect(File.exist?(ctx.hab_bin)).to be true
            expect(File.executable?(ctx.hab_bin)).to be true
        end

        it "hab-sup command should be compiled" do
            expect(File.exist?(ctx.hab_sup_bin)).to be true
            expect(File.executable?(ctx.hab_sup_bin)).to be true
        end
    end

    context "install a core package" do
        it "should return a 0 exist status" do
            pkg_path = Pathname.new(ctx.hab_pkg_path).join("core", "bc")
            # TODO: should we use a non-core package?
            # TODO: what if it already exists? do we remove it or fail?
            #       OR should that be declared up front and let inspec fail
            #       if it already exists
            # TODO: this test now depends on the Depot being up and running
            expect(File.exist?(pkg_path)).to eq false
            expect(ctx.cmd("pkg install core/bc")).to eq 0
            expect(File.exist?(pkg_path)).to eq true
        end
    end

    context "build a package" do
        it "should build without failure" do
            plan_dir = ctx.mk_temp_dir()

            FIXTURE IS MISSING AGAIN

            # TODO: don't hardcode this path
            `cp -r /src/test/fixtures/simple-service/* #{plan_dir}`
            # TODO: error handling with paths
            result = ctx.cmd("studio build #{plan_dir}")
            expect(result).not_to be_nil
            expect(result.success?).to be true
        end
    end
end

