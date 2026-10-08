    // What each cleanup mode does to the same spoken sentence.
    const CLEANUP_EXAMPLES = {
      basic: {
        said: "“Um, so I think we should, uh, ship the beta on Friday… then collect feedback.”",
        typed: "Um, so I think we should, uh, ship the beta on Friday. Then collect feedback."
      },
      "clean-dictation": {
        said: "“<s>Um,</s> so I think we should, <s>uh,</s> ship the beta on Friday… then collect feedback.”",
        typed: "So I think we should ship the beta on Friday. Then collect feedback."
      },
      "clean-dictation-with-pause-breaks": {
        said: "“<s>Um,</s> so I think we should, <s>uh,</s> ship the beta on Friday… [pause] then collect feedback.”",
        typed: "So I think we should ship the beta on Friday.\nThen collect feedback."
      }
    };
