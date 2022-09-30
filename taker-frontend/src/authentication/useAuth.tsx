import React, { createContext, ReactNode, useContext, useEffect, useMemo, useState } from "react";
import { useLocation } from "react-router-dom";
import { useNavigate } from "react-router-dom";
import * as sessionsApi from "./session";
import { Authenticated } from "./session";

interface AuthContextType {
    authenticated: boolean;
    firstLogin: boolean;
    loading: boolean;
    error?: any;
    login: (password: string) => void;
    changePassword: (password: string) => void;
    logout: () => void;
}

const AuthContext = createContext<AuthContextType>(
    {} as AuthContextType,
);

export function AuthProvider({
    children,
}: {
    children: ReactNode;
}): JSX.Element {
    const [authenticated, setAuthenticated] = useState<boolean>(false);
    const [firstLogin, setFirstLogin] = useState<boolean>(false);
    const [error, setError] = useState<any>();
    const [loading, setLoading] = useState<boolean>(false);
    const [loadingInitial, setLoadingInitial] = useState<boolean>(true);
    const navigate = useNavigate();
    const location = useLocation();

    useEffect(() => {
        setError(null);
    }, [location.pathname]);

    // Check if there is a currently active session
    // when the provider is mounted for the first time.
    //
    // If there is an error, it means there is no session.
    //
    // Finally, signal the component that the initial load
    // is over.
    useEffect(() => {
        sessionsApi.isAuthenticated()
            .then(({ authenticated, first_login }: Authenticated) => {
                setAuthenticated(authenticated);
                setFirstLogin(first_login);
                localStorage.setItem("oldUser", "true");
            })
            .catch((_error) => {
                setAuthenticated(false);
                setFirstLogin(false);
            })
            .finally(() => setLoadingInitial(false));
    }, []);

    // Flags the component loading state and posts the login
    // data to the server.
    //
    // An error means that the password is not valid.
    //
    // Finally, signal the component that loading the
    // loading state is over.
    function login(password: string) {
        setLoading(true);

        sessionsApi.login({ password })
            .then((user) => {
                if (user.first_login) {
                    setFirstLogin(true);
                    navigate("/change-password");
                } else {
                    setAuthenticated(true);
                    navigate("/");
                }
            })
            .catch((error) => {
                setError(error);
                setAuthenticated(false);
            })
            .finally(() => setLoading(false));
    }

    function changePassword(password: string) {
        setLoading(true);

        sessionsApi.changePassword({ password })
            .then(() => {
                setAuthenticated(true);
                setFirstLogin(false);
                navigate("/");
            })
            .catch((error) => {
                setError(error);
            })
            .finally(() => setLoading(false));
    }

    function logout() {
        sessionsApi.logout().then(() => setAuthenticated(false)).catch((error) => {
            console.log("Received error " + error);
            setError(error);
        });
    }

    // Make the provider update only when it should.
    // We only want to force re-renders if the user,
    // loading or error states change.
    //
    // Whenever the `value` passed into a provider changes,
    // the whole tree under the provider re-renders, and
    // that is very costly!
    const memoedValue = useMemo(
        () => ({
            authenticated,
            loading,
            error,
            firstLogin,
        }),
        [authenticated, loading, error, firstLogin],
    );

    // We only want to render the underlying app after we
    // assert for the presence of a current user.
    const val = {
        login,
        changePassword,
        logout,
        ...memoedValue,
    };
    return (
        <AuthContext.Provider value={val}>
            {!loadingInitial && children}
        </AuthContext.Provider>
    );
}

export default function useAuth() {
    return useContext(AuthContext);
}
