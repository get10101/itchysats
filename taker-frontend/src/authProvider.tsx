import * as React from "react";
import {createContext, ReactNode, useContext, useState} from "react";
import {useLocation, useNavigate} from "react-router-dom";

/**
 * Our actual authentication proder
 */
const fakeAuthProvider = {
    isAuthenticated: false,
    signin(callback: VoidFunction) {
        fakeAuthProvider.isAuthenticated = true;
        callback()
    },
    signout(callback: VoidFunction) {
        fakeAuthProvider.isAuthenticated = false;
        callback()
    },
};

interface AuthContextType {
    isAuthenticated: boolean;
    signin: (password: string, callback: VoidFunction) => void;
    signout: (callback: VoidFunction) => void;
}

let AuthContext = createContext<AuthContextType>(null!);

function AuthProvider({ children }: { children: ReactNode }) {
    let [user, setUser] = useState<string|null>();
    let navigate = useNavigate();
    let location = useLocation();
    let path = location.pathname;
    let signin = (password: string, callback: VoidFunction) => {
        return fakeAuthProvider.signin(() => {
            setUser(password);
            navigate(`itchysats:${password}@${path}`)
            callback();
        });
    };

    let signout = (callback: VoidFunction) => {
        return fakeAuthProvider.signout(() => {
            setUser(null);
            callback();
        });
    };

    let value = { user, signin, signout, isAuthenticated :false };

    return <AuthContext.Provider value={value}>{children}</AuthContext.Provider>;
}

function useAuth() {
    return useContext(AuthContext);
}

export { AuthProvider, useAuth };
