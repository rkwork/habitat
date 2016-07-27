INSPEC=hab pkg exec core/inspec inspec
all:
	hab pkg install core/inspec
	${INSPEC} exec ./hab_inspec/controls/clean_env.rb
	${INSPEC} exec ./specs/basic.rb
